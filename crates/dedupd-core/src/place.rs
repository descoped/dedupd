//! Placement: putting stored files where each app looks for them, as clones.

use crate::clone::clone_file;
use crate::db::{Db, NewPlacement};
use crate::hash::hash_file;
use crate::ollama;
use crate::store::{Fingerprint, Store};
use anyhow::{Context, Result, bail};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

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

    pub fn as_str(self) -> &'static str {
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
        })
    }

    /// All three under one folder; for tests and trials.
    pub fn under(root: &Path) -> Self {
        Self {
            bionic: root.join("lmstudio/models"),
            ollama: root.join("ollama/models"),
            hf_cache: root.join("huggingface/hub"),
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

struct Target<'a> {
    app: App,
    repo_id: i64,
    rel_path: &'a str,
    sha256: &'a str,
    size: u64,
    commit: Option<&'a str>,
}

/// Put a clone of a stored file at `dst`. An existing file is never
/// overwritten: the same content counts as present, anything else is a
/// conflict for the owner to resolve.
fn place_clone(db: &Db, store: &Store, t: &Target<'_>, dst: &Path, out: &mut Placed) -> Result<()> {
    let dst_str = dst.to_string_lossy().into_owned();
    let record = |shared: bool, origin: &str| -> Result<()> {
        let fp = Fingerprint::of(dst)?;
        db.upsert_placement(&NewPlacement {
            app: t.app.as_str(),
            path: &dst_str,
            repo_id: t.repo_id,
            rel_path: t.rel_path,
            sha256: t.sha256,
            size: fp.size,
            mtime_ns: fp.mtime_ns,
            origin,
            verify_state: if t.commit.is_some() {
                "verified"
            } else {
                "unchecked"
            },
            verified_commit: t.commit,
            shared,
        })
    };

    if let Ok(meta) = fs::symlink_metadata(dst) {
        if !meta.is_file() {
            out.conflicts
                .push(format!("{dst_str} exists and is not a regular file"));
            return Ok(());
        }
        if let Some(known) = db.placement(&dst_str)?
            && known.sha256 == t.sha256
            && Fingerprint::of(dst)?
                == (Fingerprint {
                    size: known.size,
                    mtime_ns: known.mtime_ns,
                })
        {
            out.present += 1;
            return Ok(());
        }
        if meta.len() == t.size && hash_file(dst, false)?.sha256 == t.sha256 {
            // The app has its own copy of the same bytes. Leave it; the
            // reclaim pass can swap it for a clone.
            record(false, "adopted")?;
            out.present += 1;
        } else {
            out.conflicts
                .push(format!("{dst_str} exists with different content"));
        }
        return Ok(());
    }

    if !store.ensure_local(t.sha256)? {
        bail!("{} is not in the store", t.sha256);
    }
    let parent = dst.parent().context("destination has no parent folder")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    if !crate::clone::same_volume(parent, &store.blob_path(t.sha256))? {
        bail!(
            "{} is on a different volume from the store; clones need one APFS volume",
            parent.display()
        );
    }
    let name = dst.file_name().context("destination has no file name")?;
    let tmp = parent.join(format!(".{}.dedupd-tmp", name.to_string_lossy()));
    let _ = fs::remove_file(&tmp);
    clone_file(&store.blob_path(t.sha256), &tmp)
        .with_context(|| format!("cloning into {}", parent.display()))?;
    let finish = || -> Result<()> {
        // Store files are read-only; an app's own file is not.
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
        fs::rename(&tmp, dst)?;
        Ok(())
    };
    if let Err(e) = finish() {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    record(true, "placed")?;
    out.cloned += 1;
    Ok(())
}

/// Bionic and LM Studio: `<models>/<org>/<repo>/<file>`.
pub fn place_bionic(
    db: &Db,
    store: &Store,
    dirs: &AppDirs,
    repo_id: i64,
    repo: &str,
    commit: &str,
    items: &[Item],
) -> Result<Placed> {
    let mut out = Placed::default();
    let base = dirs.bionic.join(repo);
    for item in items {
        let t = Target {
            app: App::Bionic,
            repo_id,
            rel_path: &item.rel_path,
            sha256: &item.sha256,
            size: item.size,
            commit: Some(commit),
        };
        place_clone(db, store, &t, &base.join(&item.rel_path), &mut out)?;
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
#[allow(clippy::too_many_arguments)]
pub fn place_hf(
    db: &Db,
    store: &Store,
    dirs: &AppDirs,
    repo_id: i64,
    repo: &str,
    commit: &str,
    ref_name: Option<&str>,
    items: &[Item],
) -> Result<Placed> {
    let mut out = Placed::default();
    let base = hf_repo_dir(dirs, repo);
    for item in items {
        let t = Target {
            app: App::Hf,
            repo_id,
            rel_path: &item.rel_path,
            sha256: &item.sha256,
            size: item.size,
            commit: Some(commit),
        };
        let conflicts = out.conflicts.len();
        place_clone(
            db,
            store,
            &t,
            &base.join("blobs").join(&item.etag),
            &mut out,
        )?;
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
/// layers are missing.
pub fn place_ollama(
    db: &Db,
    store: &Store,
    dirs: &AppDirs,
    repo_id: i64,
    repo: &str,
    tag: &str,
    manifest_body: &str,
) -> Result<Placed> {
    let manifest = ollama::Manifest::parse(manifest_body)?;
    let mut out = Placed::default();
    for layer in manifest.all() {
        let sha = layer.sha256()?;
        let size = match db.blob(sha)? {
            Some(b) => b.size,
            None => bail!("layer {} is not in the store", layer.digest),
        };
        let rel = format!("blobs/sha256-{sha}");
        let t = Target {
            app: App::Ollama,
            repo_id,
            rel_path: &rel,
            sha256: sha,
            size,
            commit: None,
        };
        place_clone(db, store, &t, &dirs.ollama.join(&rel), &mut out)?;
    }
    if !out.conflicts.is_empty() {
        return Ok(out);
    }
    let path = ollama_manifest_path(dirs, repo, tag);
    if fs::read_to_string(&path).ok().as_deref() != Some(manifest_body) {
        let parent = path.parent().expect("manifest has a parent");
        fs::create_dir_all(parent)?;
        let tmp = parent.join(format!(".{tag}.dedupd-tmp"));
        fs::write(&tmp, manifest_body)?;
        fs::rename(&tmp, &path)?;
    }
    Ok(out)
}
