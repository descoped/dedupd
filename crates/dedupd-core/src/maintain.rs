//! Upkeep: swapping duplicate app files for clones, checking the store, and
//! explicit removal. Nothing here runs unless the owner or the daemon asks.

use crate::adopt::PROVIDER_HF;
use crate::clone::{clone_file, same_volume};
use crate::db::{BlobRow, Db};
use crate::hash::hash_file;
use crate::store::{Fingerprint, Store};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

fn lock(db: &Mutex<Db>) -> std::sync::MutexGuard<'_, Db> {
    db.lock().expect("database lock")
}

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
        .filter(|p| !p.shared)
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
            let recorded = Fingerprint {
                size: p.size,
                mtime_ns: p.mtime_ns,
            };
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

            let parent = path.parent().context("file has no parent folder")?;
            let name = path.file_name().context("file has no name")?;
            let tmp = parent.join(format!(".{}.dedupd-tmp", name.to_string_lossy()));
            let _ = fs::remove_file(&tmp);
            clone_file(&blob, &tmp)?;
            let swap = || -> Result<()> {
                // The clone takes the place of the app's file, so it carries
                // the same permissions and modification time.
                fs::set_permissions(&tmp, meta.permissions())?;
                fs::File::open(&tmp)?.set_modified(meta.modified()?)?;
                if Fingerprint::of(path)? != recorded {
                    bail!("changed while it was being replaced");
                }
                fs::rename(&tmp, path)?;
                Ok(())
            };
            if let Err(e) = swap() {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
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
    let query = |sql: &str| -> Result<Vec<BlobRow>> {
        let mut stmt = db.conn().prepare(sql)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(BlobRow {
                    sha256: r.get(0)?,
                    size: r.get::<_, i64>(1)? as u64,
                    git_oid: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    };
    const UNPLACED: &str = "NOT EXISTS (SELECT 1 FROM placements p WHERE p.sha256 = b.sha256)";
    let orphans = query(&format!(
        "SELECT b.sha256, b.size, b.git_oid FROM blobs b WHERE {UNPLACED}
           AND NOT EXISTS (SELECT 1 FROM revision_files f
                           WHERE f.lfs_sha256 = b.sha256
                              OR (f.lfs_sha256 IS NULL AND f.git_oid = b.git_oid))
           AND NOT EXISTS (SELECT 1 FROM ollama_manifests m WHERE instr(m.body, b.sha256) > 0)
         ORDER BY b.sha256"
    ))?;
    let mut plan = GcPlan {
        orphans,
        offload: Vec::new(),
    };
    if offload && let Some(archive) = store.archive() {
        plan.offload = query(&format!(
            "SELECT b.sha256, b.size, b.git_oid FROM blobs b WHERE {UNPLACED} ORDER BY b.sha256"
        ))?
        .into_iter()
        .filter(|b| !plan.orphans.iter().any(|o| o.sha256 == b.sha256))
        .filter(|b| store.contains(&b.sha256))
        .filter(|b| fs::metadata(archive.blob_path(&b.sha256)).is_ok_and(|m| m.len() == b.size))
        .collect();
    }
    Ok(plan)
}

/// Carry out a plan. Returns the bytes freed on the local disk.
pub fn gc_apply(db: &Mutex<Db>, store: &Store, plan: &GcPlan) -> Result<u64> {
    let mut freed = 0;
    for blob in &plan.orphans {
        let path = store.blob_path(&blob.sha256);
        if path.is_file() {
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            freed += blob.size;
        }
        if let Some(archive) = store.archive() {
            let _ = fs::remove_file(archive.blob_path(&blob.sha256));
        }
        lock(db).delete_blob(&blob.sha256)?;
    }
    for blob in &plan.offload {
        let path = store.blob_path(&blob.sha256);
        if path.is_file() {
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            freed += blob.size;
        }
    }
    Ok(freed)
}

/// Drop a repository from the archive records, so that `gc` sees its files as
/// unreferenced. Refused while an app still has files of the repository.
pub fn forget(db: &Mutex<Db>, repo: &str) -> Result<()> {
    prune_placements(db)?;
    let db = lock(db);
    let Some(id) = db.repo_id(PROVIDER_HF, repo)? else {
        bail!("{repo} is not in the archive");
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
    db.conn().execute_batch(&format!(
        "BEGIN;
         DELETE FROM revision_files WHERE revision_id IN (SELECT id FROM revisions WHERE repo_id = {id});
         DELETE FROM revisions WHERE repo_id = {id};
         DELETE FROM refs WHERE repo_id = {id};
         DELETE FROM ollama_manifests WHERE repo_id = {id};
         DELETE FROM repos WHERE id = {id};
         COMMIT;"
    ))?;
    Ok(())
}
