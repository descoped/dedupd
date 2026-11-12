//! Placement: putting stored files where each app looks for them, as clones.

use crate::adopt::Verify;
use crate::clone::clone_into_place;
use crate::db::{Db, NewPlacement, ORIGIN_ADOPTED, ORIGIN_PLACED, lock};
use crate::hash::hash_file;
use crate::ollama;
use crate::store::{Fingerprint, Store};
use anyhow::{Context, Result, bail};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum App {
    Bionic,
    Ollama,
    /// The Hugging Face cache, which `huggingface_hub`, MLX tools and
    /// llama.cpp all read.
    Hf,
}

impl App {
    pub const ALL: [App; 3] = [App::Bionic, App::Ollama, App::Hf];

    /// The app's short name, as recorded in the database.
    pub const fn as_str(self) -> &'static str {
        match self {
            App::Bionic => "bionic",
            App::Ollama => "ollama",
            App::Hf => "hf",
        }
    }
}

impl std::str::FromStr for App {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.trim().to_lowercase().as_str() {
            "bionic" | "lmstudio" | "lm-studio" => App::Bionic,
            "ollama" => App::Ollama,
            "hf" | "huggingface" | "mlx" | "llama.cpp" | "llamacpp" => App::Hf,
            other => bail!("unknown app {other}; use bionic, ollama or hf (also: mlx, llama.cpp)"),
        })
    }
}

/// Where each app keeps its models.
#[derive(Debug, Clone)]
pub struct AppDirs {
    pub bionic: PathBuf,
    pub ollama: PathBuf,
    pub hf_cache: PathBuf,
    /// Where the apps themselves are installed; their versions are read
    /// from there.
    pub apps: PathBuf,
}

impl AppDirs {
    /// The folders the apps use by default, honouring the environment
    /// variables each app itself reads, then the config file.
    pub fn resolve(config: &crate::config::Config) -> Result<Self> {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        let env = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Ok(Self {
            bionic: config
                .bionic_models
                .clone()
                .unwrap_or_else(|| home.join(".lmstudio/models")),
            ollama: config
                .ollama_models
                .clone()
                .or_else(|| env("OLLAMA_MODELS"))
                .unwrap_or_else(|| home.join(".ollama/models")),
            hf_cache: config
                .hf_cache
                .clone()
                .or_else(|| env("HF_HUB_CACHE"))
                .or_else(|| env("HF_HOME").map(|h| h.join("hub")))
                .unwrap_or_else(|| home.join(".cache/huggingface/hub")),
            apps: PathBuf::from("/Applications"),
        })
    }

    /// All three under one folder; for tests and trials.
    pub fn under(root: &Path) -> Self {
        Self {
            bionic: root.join("lmstudio/models"),
            ollama: root.join("ollama/models"),
            hf_cache: root.join("huggingface/hub"),
            apps: root.join("Applications"),
        }
    }

    /// The folder an app keeps its models in.
    pub fn models(&self, app: App) -> &Path {
        match app {
            App::Bionic => &self.bionic,
            App::Ollama => &self.ollama,
            App::Hf => &self.hf_cache,
        }
    }
}

/// One file of a revision, with its content in the store.
#[derive(Debug, Clone)]
pub struct Item {
    pub rel_path: String,
    pub sha256: String,
    pub size: u64,
    /// The name the Hugging Face cache gives the file: the sha256 for an LFS
    /// file, the git blob id otherwise.
    pub etag: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Placed {
    /// Files created as clones.
    pub cloned: usize,
    /// Files that were already there with the right content.
    pub present: usize,
    /// Files that exist with other content and were left alone.
    pub conflicts: Vec<String>,
}

/// Where a repository's files are being placed, and the records to keep.
pub struct Dest<'a> {
    pub db: &'a Mutex<Db>,
    pub store: &'a Store,
    pub dirs: &'a AppDirs,
    pub repo_id: i64,
    /// `org/name`.
    pub repo: &'a str,
}

pub(crate) struct Target<'a> {
    pub(crate) app: App,
    pub(crate) rel_path: &'a str,
    pub(crate) sha256: &'a str,
    pub(crate) size: u64,
    pub(crate) commit: Option<&'a str>,
}

/// Put a clone of a stored file at `dst`. An existing file is never
/// overwritten: the same content counts as present, anything else is a
/// conflict for the owner to resolve.
///
/// The database is locked only for its own calls, never while a file is
/// hashed or brought back from the archive disk.
pub(crate) fn place_clone(
    dest: &Dest<'_>,
    t: &Target<'_>,
    dst: &Path,
    out: &mut Placed,
) -> Result<()> {
    let dst_str = dst.to_string_lossy().into_owned();
    let record = |shared: bool, origin: &str| -> Result<()> {
        let fp = Fingerprint::of(dst)?;
        let verify = if t.commit.is_some() {
            Verify::Verified
        } else {
            Verify::Unchecked
        };
        lock(dest.db).upsert_placement(&NewPlacement {
            app: t.app.as_str(),
            path: &dst_str,
            repo_id: dest.repo_id,
            rel_path: t.rel_path,
            sha256: t.sha256,
            size: fp.size,
            mtime_ns: fp.mtime_ns,
            origin,
            verify_state: verify.as_str(),
            verified_commit: t.commit,
            shared,
            derived: None,
        })
    };

    if let Ok(meta) = fs::symlink_metadata(dst) {
        if !meta.is_file() {
            out.conflicts
                .push(format!("{dst_str} exists and is not a regular file"));
            return Ok(());
        }
        let known = lock(dest.db).placement(&dst_str)?;
        if let Some(known) = known
            && known.sha256 == t.sha256
            && Fingerprint::of(dst)? == known.fingerprint()
        {
            out.present += 1;
            return Ok(());
        }
        if meta.len() == t.size && hash_file(dst, false)?.sha256 == t.sha256 {
            // The app has its own copy of the same bytes. Leave it; the
            // reclaim pass can swap it for a clone.
            record(false, ORIGIN_ADOPTED)?;
            out.present += 1;
        } else {
            out.conflicts
                .push(format!("{dst_str} exists with different content"));
        }
        return Ok(());
    }

    let store = dest.store;
    if !store.ensure_local(t.sha256)? {
        bail!("{} is not in the store", t.sha256);
    }
    let parent = dst.parent().context("destination has no parent folder")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let blob = store.blob_path(t.sha256);
    if !crate::clone::same_volume(parent, &blob)? {
        bail!(
            "{} is on a different volume from the store; clones need one APFS volume",
            parent.display()
        );
    }
    let mut cloned = false;
    let placed = clone_into_place(&blob, dst, |tmp| {
        cloned = true;
        // Store files are read-only; an app's own file is not.
        fs::set_permissions(tmp, fs::Permissions::from_mode(0o644))?;
        Ok(())
    });
    match placed {
        Ok(()) => {}
        Err(e) if cloned => return Err(e),
        Err(e) => return Err(e.context(format!("cloning into {}", parent.display()))),
    }
    record(true, ORIGIN_PLACED)?;
    out.cloned += 1;
    Ok(())
}

/// Bionic and LM Studio: `<models>/<org>/<repo>/<file>`.
pub fn place_bionic(dest: &Dest<'_>, commit: &str, items: &[Item]) -> Result<Placed> {
    let mut out = Placed::default();
    let base = dest.dirs.bionic.join(dest.repo);
    for item in items {
        let t = Target {
            app: App::Bionic,
            rel_path: &item.rel_path,
            sha256: &item.sha256,
            size: item.size,
            commit: Some(commit),
        };
        place_clone(dest, &t, &base.join(&item.rel_path), &mut out)?;
    }
    Ok(out)
}

/// The folder the Hugging Face cache uses for a model repository.
pub fn hf_repo_dir(dirs: &AppDirs, repo: &str) -> PathBuf {
    dirs.hf_cache
        .join(format!("models--{}", repo.replace('/', "--")))
}

/// The Hugging Face cache: `blobs/<etag>`, a `snapshots/<commit>/<path>` link
/// to each blob, and `refs/<branch>` naming the commit.
pub fn place_hf(
    dest: &Dest<'_>,
    commit: &str,
    ref_name: Option<&str>,
    items: &[Item],
) -> Result<Placed> {
    let mut out = Placed::default();
    let base = hf_repo_dir(dest.dirs, dest.repo);
    for item in items {
        let t = Target {
            app: App::Hf,
            rel_path: &item.rel_path,
            sha256: &item.sha256,
            size: item.size,
            commit: Some(commit),
        };
        let conflicts = out.conflicts.len();
        place_clone(dest, &t, &base.join("blobs").join(&item.etag), &mut out)?;
        if out.conflicts.len() > conflicts {
            continue;
        }
        let link = base.join("snapshots").join(commit).join(&item.rel_path);
        if fs::symlink_metadata(&link).is_err() {
            let depth = item.rel_path.matches('/').count();
            let target = format!("{}blobs/{}", "../".repeat(depth + 2), item.etag);
            fs::create_dir_all(link.parent().expect("link has a parent"))?;
            std::os::unix::fs::symlink(&target, &link)
                .with_context(|| format!("linking {}", link.display()))?;
        }
    }
    if let Some(name) = ref_name {
        let path = base.join("refs").join(name);
        fs::create_dir_all(path.parent().expect("ref has a parent"))?;
        fs::write(&path, commit)?;
    }
    Ok(out)
}

/// Where Ollama keeps the manifest for `hf.co/<repo>:<tag>`.
pub fn ollama_manifest_path(dirs: &AppDirs, repo: &str, tag: &str) -> PathBuf {
    dirs.ollama
        .join("manifests")
        .join(ollama::HOST)
        .join(repo)
        .join(tag)
}

/// Ollama: each layer as `blobs/sha256-<hex>`, then the manifest that names
/// them. The manifest is written last so Ollama never sees a model whose
/// layers are missing. `manifest` is `manifest_body` parsed; the body is
/// written as it is.
pub fn place_ollama(
    dest: &Dest<'_>,
    tag: &str,
    manifest: &ollama::Manifest,
    manifest_body: &str,
) -> Result<Placed> {
    let mut out = Placed::default();
    for layer in manifest.all() {
        let sha = layer.sha256()?;
        let known = lock(dest.db).blob(sha)?;
        let size = match known {
            Some(b) => b.size,
            None => bail!("layer {} is not in the store", layer.digest),
        };
        let rel = format!("blobs/sha256-{sha}");
        let t = Target {
            app: App::Ollama,
            rel_path: &rel,
            sha256: sha,
            size,
            commit: None,
        };
        place_clone(dest, &t, &dest.dirs.ollama.join(&rel), &mut out)?;
    }
    if !out.conflicts.is_empty() {
        return Ok(out);
    }
    let path = ollama_manifest_path(dest.dirs, dest.repo, tag);
    if fs::read_to_string(&path).ok().as_deref() != Some(manifest_body) {
        let parent = path.parent().expect("manifest has a parent");
        fs::create_dir_all(parent)?;
        let tmp = parent.join(format!(".{tag}.dedupd-tmp"));
        fs::write(&tmp, manifest_body)?;
        fs::rename(&tmp, &path)?;
    }
    Ok(out)
}
