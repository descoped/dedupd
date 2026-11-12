//! Upkeep: swapping duplicate app files for clones, checking the store, and
//! explicit removal. Nothing here runs unless the owner or the daemon asks.

use crate::clone::{clone_into_place, same_volume};
use crate::db::{BlobRow, Db, PROVIDER_HF, lock};
use crate::hash::hash_file;
use crate::place::{App, AppDirs, hf_repo_dir};
use crate::store::{Fingerprint, Store};
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

/// How long a duplicate must have been left alone before `reclaim` swaps it
/// for a clone, so a file an app is still writing is not touched.
pub const RECLAIM_MIN_AGE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ReclaimReport {
    /// App files replaced by a clone of the stored file.
    pub swapped: usize,
    /// Disk space those files were using on their own.
    pub bytes: u64,
    /// Files left alone, as `(path, reason)`.
    pub skipped: Vec<(String, String)>,
}

/// Replace app files that are separate copies of stored content with clones
/// of the stored file, which frees the copy's disk space.
///
/// A file is only touched when it is exactly as it was when it was hashed and
/// has not been written for `min_age`. The swap is a rename, so a program
/// that has the old file open keeps reading it undisturbed.
pub fn reclaim(
    db: &Mutex<Db>,
    store: &Store,
    min_age: Duration,
    dry_run: bool,
) -> Result<ReclaimReport> {
    let mut report = ReclaimReport::default();
    let candidates: Vec<_> = lock(db)
        .placements()?
        .into_iter()
        // A derived file holds other bytes than the stored one on purpose.
        .filter(|p| !p.shared && p.derived_sha256.is_none())
        .collect();
    for p in candidates {
        let path = Path::new(&p.path);
        let result = (|| -> Result<Option<String>> {
            let Ok(meta) = fs::symlink_metadata(path) else {
                return Ok(Some("the file is gone".into()));
            };
            if !meta.is_file() {
                return Ok(Some("not a regular file".into()));
            }
            let recorded = p.fingerprint();
            if Fingerprint::of(path)? != recorded {
                return Ok(Some("changed since it was hashed".into()));
            }
            let age = SystemTime::now()
                .duration_since(meta.modified()?)
                .unwrap_or_default();
            if age < min_age {
                return Ok(Some("written too recently".into()));
            }
            if !store.contains(&p.sha256) {
                return Ok(Some("its content is not in the local store".into()));
            }
            let blob = store.blob_path(&p.sha256);
            if !same_volume(path, &blob)? {
                return Ok(Some("on a different volume from the store".into()));
            }
            if dry_run {
                return Ok(None);
            }

            clone_into_place(&blob, path, |tmp| {
                // The clone takes the place of the app's file, so it carries
                // the same permissions and modification time.
                fs::set_permissions(tmp, meta.permissions())?;
                fs::File::open(tmp)?.set_modified(meta.modified()?)?;
                if Fingerprint::of(path)? != recorded {
                    bail!("changed while it was being replaced");
                }
                Ok(())
            })?;
            lock(db).set_placement_shared(&p.path, Fingerprint::of(path)?.mtime_ns)?;
            Ok(None)
        })();
        match result {
            Ok(None) => {
                report.swapped += 1;
                report.bytes += p.size;
            }
            Ok(Some(reason)) => report.skipped.push((p.path, reason)),
            Err(e) => report.skipped.push((p.path, format!("{e:#}"))),
        }
    }
    Ok(report)
}

/// Forget app files that no longer exist. Returns how many were forgotten.
/// The stored content stays; only `gc` removes anything from the store.
pub fn prune_placements(db: &Mutex<Db>) -> Result<usize> {
    let gone: Vec<String> = lock(db)
        .placements()?
        .into_iter()
        .filter(|p| fs::symlink_metadata(&p.path).is_err())
        .map(|p| p.path)
        .collect();
    let db = lock(db);
    for path in &gone {
        db.delete_placement(path)?;
    }
    Ok(gone.len())
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct VerifyReport {
    pub checked: usize,
    pub bytes: u64,
    /// Files whose content no longer hashes to their name.
    pub damaged: Vec<String>,
    /// Files the database lists that are in neither the store nor the archive.
    pub missing: Vec<String>,
    /// Files that are only on the archive disk.
    pub only_in_archive: usize,
}

/// Hash every stored file and compare with its name. With `archive`, check
/// the archive disk's copies instead of the local ones.
pub fn verify(db: &Mutex<Db>, store: &Store, archive: bool, jobs: usize) -> Result<VerifyReport> {
    let blobs = lock(db).all_blobs()?;
    let root = match (archive, store.archive()) {
        (true, Some(a)) if !crate::archive::attached(a) => bail!(
            "the archive disk is not attached ({} not found)",
            a.root().display()
        ),
        (true, Some(a)) => a.clone(),
        (true, None) => bail!("no archive folder is configured"),
        (false, _) => store.home().clone(),
    };
    let other = if archive { None } else { store.archive() };

    let next = AtomicUsize::new(0);
    let report = Mutex::new(VerifyReport::default());
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(blob) = blobs.get(i) else { break };
                    let path = root.blob_path(&blob.sha256);
                    if !path.is_file() {
                        let mut r = report.lock().expect("lock");
                        if other.is_some_and(|a| a.blob_path(&blob.sha256).is_file()) {
                            r.only_in_archive += 1;
                        } else {
                            r.missing.push(blob.sha256.clone());
                        }
                        continue;
                    }
                    let ok = hash_file(&path, false).is_ok_and(|h| h.sha256 == blob.sha256);
                    let mut r = report.lock().expect("lock");
                    r.checked += 1;
                    r.bytes += blob.size;
                    if !ok {
                        r.damaged.push(blob.sha256.clone());
                    }
                }
            });
        }
    });
    let mut report = report.into_inner().expect("lock");
    report.damaged.sort();
    report.missing.sort();
    Ok(report)
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct GcPlan {
    /// Stored files that no archived revision, Ollama manifest or app file
    /// refers to. Removing them loses nothing dedupd knows a use for.
    pub orphans: Vec<BlobRow>,
    /// Stored files that are safely on the archive disk and that no app
    /// uses. Removing the local copy frees space; they come back on demand.
    pub offload: Vec<BlobRow>,
}

/// Work out what `gc` would remove. Removes nothing.
pub fn gc_plan(db: &Mutex<Db>, store: &Store, offload: bool) -> Result<GcPlan> {
    let db = lock(db);
    let mut plan = GcPlan {
        orphans: db.unreferenced_blobs()?,
        offload: Vec::new(),
    };
    if offload && let Some(archive) = store.archive() {
        let orphans: HashSet<&str> = plan.orphans.iter().map(|o| o.sha256.as_str()).collect();
        plan.offload = db
            .unplaced_blobs()?
            .into_iter()
            .filter(|b| !orphans.contains(b.sha256.as_str()))
            .filter(|b| store.contains(&b.sha256))
            .filter(|b| fs::metadata(archive.blob_path(&b.sha256)).is_ok_and(|m| m.len() == b.size))
            .collect();
    }
    Ok(plan)
}

/// Carry out a plan. Returns the bytes freed on the local disk.
pub fn gc_apply(db: &Mutex<Db>, store: &Store, plan: &GcPlan) -> Result<u64> {
    // Remove the local copy; the bytes that freed.
    let remove_local = |blob: &BlobRow| -> Result<u64> {
        let path = store.blob_path(&blob.sha256);
        if !path.is_file() {
            return Ok(0);
        }
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        Ok(blob.size)
    };
    let mut freed = 0;
    for blob in &plan.orphans {
        freed += remove_local(blob)?;
        if let Some(archive) = store.archive() {
            let _ = fs::remove_file(archive.blob_path(&blob.sha256));
        }
        lock(db).delete_blob(&blob.sha256)?;
    }
    for blob in &plan.offload {
        freed += remove_local(blob)?;
    }
    Ok(freed)
}

/// Drop a repository from the library records, so that `gc` sees its files as
/// unreferenced. Refused while an app still has files of the repository.
pub fn forget(db: &Mutex<Db>, repo: &str) -> Result<()> {
    prune_placements(db)?;
    let mut db = lock(db);
    let Some(id) = db.repo_id(PROVIDER_HF, repo)? else {
        bail!("{repo} is not in the library");
    };
    let in_use: Vec<String> = db
        .placements()?
        .into_iter()
        .filter(|p| p.repo == repo)
        .map(|p| p.path)
        .collect();
    if let Some(first) = in_use.first() {
        bail!(
            "{repo} still has {} files in app folders (for example {first}). Remove the model from the apps first.",
            in_use.len()
        );
    }
    db.delete_repo(id)
}

/// What removing a repository from one app would do. Nothing in it is
/// removed until `unplace_apply` is given the plan.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct UnplacePlan {
    pub repo: String,
    pub app: String,
    /// The app's files that would be removed, as `(path, size)`. Each is
    /// exactly as it was when it was hashed, and its content stays in the
    /// store.
    pub remove: Vec<(String, u64)>,
    /// Links and manifests that go with them.
    pub links: Vec<String>,
    /// Files that would be left alone, as `(path, reason)`.
    pub keep: Vec<(String, String)>,
    pub bytes: u64,
}

/// Work out what removing `repo` from `app` would delete. The store keeps
/// every file, so the model can be placed again later.
pub fn unplace_plan(
    db: &Mutex<Db>,
    store: &Store,
    dirs: &AppDirs,
    repo: &str,
    app: App,
) -> Result<UnplacePlan> {
    let mut plan = UnplacePlan {
        repo: repo.to_string(),
        app: app.as_str().to_string(),
        ..Default::default()
    };
    let placed: Vec<_> = lock(db)
        .placements()?
        .into_iter()
        .filter(|p| p.repo == repo && p.app == app.as_str())
        .collect();
    let archive = store.archive().filter(|a| crate::archive::attached(a));
    // Ollama keeps one file per layer, and models share layers.
    let (manifests, used_elsewhere) = match app {
        App::Ollama => ollama_use(dirs, repo),
        _ => Default::default(),
    };
    for p in placed {
        let reason = match Fingerprint::of(Path::new(&p.path)) {
            Err(_) => Some("it is no longer there"),
            Ok(now) if now != p.fingerprint() => Some("it has changed since dedupd looked at it"),
            // Ollama names a layer by the bytes of the file itself.
            Ok(_) if used_elsewhere.contains(p.derived_sha256.as_ref().unwrap_or(&p.sha256)) => {
                Some("another Ollama model uses it")
            }
            Ok(_)
                if !store.contains(&p.sha256)
                    && !archive.is_some_and(|a| a.blob_path(&p.sha256).is_file()) =>
            {
                Some("the store has no copy of it")
            }
            Ok(_) => None,
        };
        match reason {
            Some(why) => plan.keep.push((p.path, why.to_string())),
            None => {
                plan.bytes += p.size;
                plan.remove.push((p.path, p.size));
            }
        }
    }
    plan.links = match app {
        App::Ollama => manifests,
        App::Hf => {
            let going: HashSet<&str> = plan.remove.iter().map(|(p, _)| p.as_str()).collect();
            hf_links(&hf_repo_dir(dirs, repo), &going)
        }
        App::Bionic => Vec::new(),
    };
    Ok(plan)
}

/// The manifest files of `repo` in Ollama's folder, and the layers that
/// other models' manifests name.
fn ollama_use(dirs: &AppDirs, repo: &str) -> (Vec<String>, HashSet<String>) {
    let mut own = Vec::new();
    let mut elsewhere = HashSet::new();
    for found in crate::sources::ollama::manifests(&dirs.ollama) {
        if found.repo == repo {
            let rel = match found.provider {
                PROVIDER_HF => Path::new(crate::ollama::HOST).join(repo),
                _ => PathBuf::from(repo),
            }
            .join(&found.tag);
            for tree in ["manifests", "manifests-v2"] {
                let path = dirs.ollama.join(tree).join(&rel);
                if fs::symlink_metadata(&path).is_ok() {
                    own.push(path.to_string_lossy().into_owned());
                }
            }
        } else if let Ok(manifest) = crate::ollama::Manifest::parse(&found.body) {
            elsewhere.extend(
                manifest
                    .all()
                    .filter_map(|l| l.sha256().ok())
                    .map(str::to_string),
            );
        }
    }
    (own, elsewhere)
}

/// The snapshot links in a Hugging Face cache folder that point at one of
/// the blobs in `going`.
fn hf_links(repo_dir: &Path, going: &HashSet<&str>) -> Vec<String> {
    let blobs = repo_dir.join("blobs");
    walkdir::WalkDir::new(repo_dir.join("snapshots"))
        .sort_by_file_name()
        .into_iter()
        .flatten()
        .filter(|e| e.path_is_symlink())
        .filter(|e| {
            fs::read_link(e.path())
                .ok()
                .and_then(|target| target.file_name().map(|name| blobs.join(name)))
                .is_some_and(|blob| going.contains(blob.to_string_lossy().as_ref()))
        })
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect()
}

/// Carry out a plan made by `unplace_plan`. A file that has changed since
/// the plan was made is left alone. Returns the files removed.
pub fn unplace_apply(db: &Mutex<Db>, dirs: &AppDirs, plan: &UnplacePlan) -> Result<usize> {
    let app: App = plan.app.parse()?;
    let recorded: std::collections::HashMap<String, Fingerprint> = lock(db)
        .placements()?
        .into_iter()
        .filter(|p| p.repo == plan.repo && p.app == plan.app)
        .map(|p| (p.path.clone(), p.fingerprint()))
        .collect();
    let mut removed = 0;
    for (path, _) in &plan.remove {
        let unchanged = recorded
            .get(path)
            .is_some_and(|fp| Fingerprint::of(Path::new(path)).is_ok_and(|now| now == *fp));
        if !unchanged {
            continue;
        }
        fs::remove_file(path).with_context(|| format!("removing {path}"))?;
        lock(db).delete_placement(path)?;
        removed += 1;
    }
    for link in &plan.links {
        let _ = fs::remove_file(link);
    }
    if app == App::Hf {
        let repo_dir = hf_repo_dir(dirs, &plan.repo);
        if repo_dir.is_dir() && is_empty_tree(&repo_dir.join("blobs")) {
            // Only names are left: which commit a branch was at, and which
            // files the provider said do not exist.
            let _ = fs::remove_dir_all(&repo_dir);
        }
    }
    // Leave no empty folders behind. The app's own folder stays, and so do
    // the folders Ollama keeps directly in it.
    let (stop, keep_top) = match app {
        App::Bionic => (dirs.bionic.as_path(), false),
        App::Hf => (dirs.hf_cache.as_path(), false),
        App::Ollama => (dirs.ollama.as_path(), true),
    };
    let mut folders: Vec<PathBuf> = plan
        .remove
        .iter()
        .map(|(p, _)| p)
        .chain(&plan.links)
        .filter_map(|p| Path::new(p).parent().map(Path::to_path_buf))
        .collect();
    folders.sort();
    folders.dedup();
    for folder in folders.iter().rev() {
        let mut dir = folder.as_path();
        while dir != stop && dir.starts_with(stop) {
            if keep_top && dir.parent() == Some(stop) {
                break;
            }
            if !remove_if_empty(dir) {
                break;
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => break,
            }
        }
    }
    Ok(removed)
}

/// Remove `dir` when nothing is in it but what Finder leaves behind.
fn remove_if_empty(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    let names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
    if names.iter().any(|n| n != ".DS_Store") {
        return false;
    }
    let _ = fs::remove_file(dir.join(".DS_Store"));
    fs::remove_dir(dir).is_ok()
}

/// No file anywhere under `dir`; also true when `dir` is not there.
fn is_empty_tree(dir: &Path) -> bool {
    !walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .any(|e| !e.file_type().is_dir())
}
