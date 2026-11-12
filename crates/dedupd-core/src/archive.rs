//! The second copy of the store, on another disk. It has the same layout as
//! the home folder, so it can be read with `--home` on any Mac.
//!
//! Two things happen here. The backup: every stored file is copied to the
//! archive disk when it is downloaded or adopted (`sync`). And archiving one
//! model, which the owner asks for: the model is taken out of every app and
//! its files leave the internal disk, so only the archive disk has them
//! (`archive_plan`, `archive_apply`); `restore` brings them back.

use crate::db::{BlobRow, Db, PROVIDER_HF, lock};
use crate::hash::{copy_hashing, hash_file};
use crate::maintain::{self, UnplacePlan};
use crate::paths::Home;
use crate::place::{App, AppDirs};
use crate::store::Store;
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SyncReport {
    pub copied: usize,
    pub copied_bytes: u64,
    /// Files the archive already had.
    pub present: usize,
    /// Files the database lists that are only in the archive now.
    pub only_in_archive: usize,
    /// Files that could not be copied, as `(sha256, reason)`.
    pub failed: Vec<(String, String)>,
}

/// True when the disk the archive lives on is attached.
pub fn attached(archive: &Home) -> bool {
    archive.root().is_dir() || archive.root().parent().is_some_and(|p| p.is_dir())
}

/// `sync` to the store's own archive folder, when one is configured and its
/// disk is attached; `None` otherwise.
pub fn sync_if_attached(
    db: &std::sync::Mutex<Db>,
    store: &Store,
    on_copy: &mut dyn FnMut(&str, u64),
) -> Option<Result<SyncReport>> {
    let archive = store.archive().filter(|a| attached(a))?;
    Some(sync(db, store, archive, on_copy))
}

/// Copy every stored file the archive lacks, then a consistent copy of the
/// database. Each file is hashed as it is written; a file that does not hash
/// to its name is not kept.
pub fn sync(
    db: &std::sync::Mutex<Db>,
    store: &Store,
    archive: &Home,
    on_copy: &mut dyn FnMut(&str, u64),
) -> Result<SyncReport> {
    if !attached(archive) {
        bail!(
            "the archive disk is not attached ({} not found)",
            archive.root().display()
        );
    }
    fs::create_dir_all(archive.tmp_dir())
        .with_context(|| format!("creating {}", archive.root().display()))?;

    let mut report = SyncReport::default();
    let blobs = lock(db).all_blobs()?;
    for blob in &blobs {
        let src = store.blob_path(&blob.sha256);
        let dst = archive.blob_path(&blob.sha256);
        let in_store = src.is_file();
        let in_archive = fs::metadata(&dst).ok();
        if !in_store && in_archive.as_ref().is_some_and(|m| m.is_file()) {
            report.only_in_archive += 1;
        }
        if in_archive.is_some_and(|m| m.len() == blob.size) {
            report.present += 1;
            continue;
        }
        if !in_store {
            report.failed.push((
                blob.sha256.clone(),
                "missing from the store and from the archive".into(),
            ));
            continue;
        }
        match copy_in(store, archive, &blob.sha256, false) {
            Ok(()) => {
                report.copied += 1;
                report.copied_bytes += blob.size;
                on_copy(&blob.sha256, blob.size);
            }
            Err(e) => report.failed.push((blob.sha256.clone(), format!("{e:#}"))),
        }
    }

    // An idle pass leaves the archive's database copy alone.
    let newer = |a: &std::path::Path, b: &std::path::Path| {
        let m = |p: &std::path::Path| fs::metadata(p).and_then(|m| m.modified()).ok();
        match (m(a), m(b)) {
            (Some(x), Some(y)) => x > y,
            _ => true,
        }
    };
    let local_db = store.home().db_path();
    let wal = local_db.with_extension("sqlite-wal");
    if report.copied == 0
        && !newer(&local_db, &archive.db_path())
        && !(wal.is_file() && newer(&wal, &archive.db_path()))
    {
        return Ok(report);
    }
    let tmp = archive.root().join("dedupd.sqlite.tmp");
    let _ = fs::remove_file(&tmp);
    lock(db)
        .vacuum_into(&tmp)
        .context("copying the database to the archive")?;
    fs::rename(&tmp, archive.db_path())?;
    Ok(report)
}

/// One copy to the archive at a time in this process: the daemon's backup
/// and an archive the owner asked for may want the same file.
static COPYING: Mutex<()> = Mutex::new(());

/// Copy one stored file to the archive, hashing it as it is written. A copy
/// that does not hash to its name is not kept. A copy of the right size
/// that is already there is left alone, unless `replace` says it is bad.
fn copy_in(store: &Store, archive: &Home, sha256: &str, replace: bool) -> Result<()> {
    let (src, dst) = (store.blob_path(sha256), archive.blob_path(sha256));
    let _one = COPYING.lock().unwrap_or_else(|e| e.into_inner());
    // Another caller may have written it while this one waited.
    let same_size = |a: &std::path::Path, b: &std::path::Path| matches!((fs::metadata(a), fs::metadata(b)), (Ok(x), Ok(y)) if x.len() == y.len());
    if !replace && same_size(&src, &dst) {
        return Ok(());
    }
    fs::create_dir_all(archive.tmp_dir())
        .with_context(|| format!("creating {}", archive.root().display()))?;
    let tmp = archive
        .tmp_dir()
        .join(format!("{sha256}.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let result = (|| -> Result<()> {
        let hashed = copy_hashing(&src, &tmp)?;
        if hashed != sha256 {
            bail!("the store file is damaged: it hashes to {hashed}");
        }
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o444))?;
        fs::create_dir_all(dst.parent().expect("blob path has a parent"))?;
        let _ = fs::remove_file(&dst);
        fs::rename(&tmp, &dst)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// The store's archive folder, when one is set and its disk is attached.
fn attached_archive(store: &Store) -> Result<&Home> {
    let Some(archive) = store.archive() else {
        bail!("no archive folder is set (`archive` in config.toml)");
    };
    if !attached(archive) {
        bail!(
            "the archive disk is not attached ({} not found)",
            archive.root().display()
        );
    }
    Ok(archive)
}

fn repo_id(db: &Db, repo: &str) -> Result<i64> {
    match db.repo_id(PROVIDER_HF, repo)? {
        Some(id) => Ok(id),
        None => bail!("{repo} is not in the library"),
    }
}

/// A stored file that archiving a model leaves on the internal disk.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Kept {
    pub sha256: String,
    pub size: u64,
    pub why: String,
}

/// What archiving one model would do. Nothing happens until `archive_apply`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ArchivePlan {
    pub repo: String,
    /// The archive folder.
    pub archive: String,
    /// What taking the model out of each app that has it would do.
    pub deactivate: Vec<UnplacePlan>,
    /// Stored files that would leave the internal disk.
    pub remove: Vec<BlobRow>,
    pub bytes: u64,
    /// Of those, the files the archive disk does not have yet. They are
    /// copied there first.
    pub copy_files: u64,
    pub copy_bytes: u64,
    /// Stored files that stay on the internal disk, and why.
    pub keep: Vec<Kept>,
}

/// Work out what archiving `repo` would do: take it out of every app, and
/// remove its files from the internal disk, except those another model or an
/// app still uses. Changes nothing.
pub fn archive_plan(
    db: &Mutex<Db>,
    store: &Store,
    dirs: &AppDirs,
    repo: &str,
) -> Result<ArchivePlan> {
    let archive = attached_archive(store)?;
    maintain::prune_placements(db)?;
    let id = repo_id(&lock(db), repo)?;
    let mut plan = ArchivePlan {
        repo: repo.to_string(),
        archive: archive.root().to_string_lossy().into_owned(),
        ..Default::default()
    };
    let placements = lock(db).placements()?;
    for app in App::ALL {
        if placements
            .iter()
            .any(|p| p.repo == repo && p.app == app.as_str())
        {
            plan.deactivate
                .push(maintain::unplace_plan(db, store, dirs, repo, app)?);
        }
    }
    // What the apps would still hold once the model is out of them.
    let going: HashSet<&str> = plan
        .deactivate
        .iter()
        .flat_map(|d| d.remove.iter().map(|(path, _)| path.as_str()))
        .collect();
    let in_apps: HashSet<&str> = placements
        .iter()
        .filter(|p| !going.contains(p.path.as_str()))
        .map(|p| p.sha256.as_str())
        .collect();
    let (blobs, elsewhere) = {
        let db = lock(db);
        (db.repo_blobs(id)?, db.repo_blobs_used_elsewhere(id)?)
    };
    let elsewhere: HashSet<&str> = elsewhere.iter().map(|b| b.sha256.as_str()).collect();
    for blob in blobs {
        if !store.contains(&blob.sha256) {
            continue;
        }
        let why = if elsewhere.contains(blob.sha256.as_str()) {
            Some("another model uses it")
        } else if in_apps.contains(blob.sha256.as_str()) {
            Some("an app still has it")
        } else {
            None
        };
        match why {
            Some(why) => plan.keep.push(Kept {
                sha256: blob.sha256,
                size: blob.size,
                why: why.to_string(),
            }),
            None => {
                let backed_up = fs::metadata(archive.blob_path(&blob.sha256))
                    .is_ok_and(|m| m.len() == blob.size);
                if !backed_up {
                    plan.copy_files += 1;
                    plan.copy_bytes += blob.size;
                }
                plan.bytes += blob.size;
                plan.remove.push(blob);
            }
        }
    }
    Ok(plan)
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ArchiveReport {
    pub repo: String,
    /// Stored files removed from the internal disk, and their size.
    pub files: u64,
    pub freed: u64,
    /// Files copied to the archive disk first, because it lacked them.
    pub copied: u64,
    /// App files removed, as `(app, files)`.
    pub deactivated: Vec<(String, usize)>,
    /// Stored files left on the internal disk, and why.
    pub keep: Vec<Kept>,
}

/// Make sure the archive holds a good copy of a stored file: hash the copy
/// that is there, and write a new one from the store if there is none or it
/// is damaged. Returns whether a copy was written.
fn secure(store: &Store, archive: &Home, blob: &BlobRow) -> Result<bool> {
    let dst = archive.blob_path(&blob.sha256);
    let there = fs::metadata(&dst).is_ok_and(|m| m.len() == blob.size);
    if there {
        let found = hash_file(&dst, false)
            .with_context(|| format!("reading {}", dst.display()))?
            .sha256;
        if found == blob.sha256 {
            return Ok(false);
        }
    }
    // A copy that is there and bad is written again; one that is not there
    // may be on its way from the daemon's backup.
    copy_in(store, archive, &blob.sha256, there)?;
    Ok(true)
}

/// Archive one model: check that the archive disk holds every file that is
/// about to leave the internal disk (reading each copy in full), take the
/// model out of every app, remove those files from the store and mark the
/// model as archived. Nothing is removed unless every copy is good.
/// `on_file` is told the size of each file as its copy has been checked.
pub fn archive_apply(
    db: &Mutex<Db>,
    store: &Store,
    dirs: &AppDirs,
    repo: &str,
    on_file: &mut dyn FnMut(u64),
) -> Result<ArchiveReport> {
    let plan = archive_plan(db, store, dirs, repo)?;
    let archive = attached_archive(store)?;
    let id = repo_id(&lock(db), repo)?;
    let mut report = ArchiveReport {
        repo: plan.repo.clone(),
        ..Default::default()
    };
    let mut failed = Vec::new();
    for blob in &plan.remove {
        match secure(store, archive, blob) {
            Ok(copied) => report.copied += u64::from(copied),
            Err(e) => failed.push(format!("{}: {e:#}", &blob.sha256[..12])),
        }
        on_file(blob.size);
    }
    if let Some(first) = failed.first() {
        bail!(
            "{} of {} files have no good copy on the archive disk, so nothing was changed ({first})",
            failed.len(),
            plan.remove.len()
        );
    }

    for step in &plan.deactivate {
        let removed = maintain::unplace_apply(db, dirs, step)?;
        report.deactivated.push((step.app.clone(), removed));
    }
    // An app file that could not be removed keeps its stored file here.
    let in_apps: HashSet<String> = lock(db)
        .placements()?
        .into_iter()
        .map(|p| p.sha256)
        .collect();
    report.keep = plan.keep;
    for blob in plan.remove {
        if in_apps.contains(&blob.sha256) {
            report.keep.push(Kept {
                sha256: blob.sha256,
                size: blob.size,
                why: "an app still has it".to_string(),
            });
            continue;
        }
        let path = store.blob_path(&blob.sha256);
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        report.files += 1;
        report.freed += blob.size;
    }
    lock(db).set_archived(id, Some(crate::now()))?;
    Ok(report)
}

/// What restoring one model would do.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RestorePlan {
    pub repo: String,
    /// The archive folder.
    pub archive: String,
    /// Files that would be copied back to the internal disk.
    pub copy: Vec<BlobRow>,
    pub bytes: u64,
    /// Free space on the internal disk.
    pub free: u64,
    /// Files that are neither on the internal disk nor on the archive disk.
    pub missing: Vec<BlobRow>,
}

/// Work out what restoring `repo` would copy back. Changes nothing.
pub fn restore_plan(db: &Mutex<Db>, store: &Store, repo: &str) -> Result<RestorePlan> {
    let archive = attached_archive(store)?;
    let blobs = {
        let db = lock(db);
        db.repo_blobs(repo_id(&db, repo)?)?
    };
    let mut plan = RestorePlan {
        repo: repo.to_string(),
        archive: archive.root().to_string_lossy().into_owned(),
        free: store.free_bytes()?,
        ..Default::default()
    };
    for blob in blobs {
        if store.contains(&blob.sha256) {
            continue;
        }
        if archive.blob_path(&blob.sha256).is_file() {
            plan.bytes += blob.size;
            plan.copy.push(blob);
        } else {
            plan.missing.push(blob);
        }
    }
    Ok(plan)
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RestoreReport {
    pub repo: String,
    pub files: u64,
    pub bytes: u64,
    /// Files that are neither on the internal disk nor on the archive disk.
    pub missing: Vec<BlobRow>,
}

/// Restore one model: copy its files back from the archive disk, hashing
/// each as it is written, and mark the model as no longer archived. The
/// archive keeps its copies. The model is not put into any app.
pub fn restore(
    db: &Mutex<Db>,
    store: &Store,
    repo: &str,
    on_file: &mut dyn FnMut(u64),
) -> Result<RestoreReport> {
    let plan = restore_plan(db, store, repo)?;
    if plan.bytes > plan.free {
        bail!(
            "restoring {repo} needs {} bytes and the internal disk has {} free",
            plan.bytes,
            plan.free
        );
    }
    let mut report = RestoreReport {
        repo: plan.repo,
        missing: plan.missing,
        ..Default::default()
    };
    for blob in &plan.copy {
        if !store.ensure_local(&blob.sha256)? {
            bail!("{} is gone from the archive disk", &blob.sha256[..12]);
        }
        report.files += 1;
        report.bytes += blob.size;
        on_file(blob.size);
    }
    let db = lock(db);
    db.set_archived(repo_id(&db, repo)?, None)?;
    Ok(report)
}
