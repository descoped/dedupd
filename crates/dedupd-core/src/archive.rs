//! The second copy of the store, on another disk. It has the same layout as
//! the home folder, so it can be read with `--home` on any Mac.

use crate::db::Db;
use crate::hash::copy_hashing;
use crate::paths::Home;
use crate::store::Store;
use anyhow::{Context, Result, bail};
use std::fs;
use std::os::unix::fs::PermissionsExt;

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
    let blobs = db.lock().expect("database lock").all_blobs()?;
    for blob in &blobs {
        let src = store.blob_path(&blob.sha256);
        let dst = archive.blob_path(&blob.sha256);
        if fs::metadata(&dst).is_ok_and(|m| m.len() == blob.size) {
            report.present += 1;
            continue;
        }
        if !src.is_file() {
            report.failed.push((
                blob.sha256.clone(),
                "missing from the store and from the archive".into(),
            ));
            continue;
        }
        let tmp = archive
            .tmp_dir()
            .join(format!("{}.{}", blob.sha256, std::process::id()));
        let _ = fs::remove_file(&tmp);
        let result = (|| -> Result<()> {
            let hashed = copy_hashing(&src, &tmp)?;
            if hashed != blob.sha256 {
                bail!("the store file is damaged: it hashes to {hashed}");
            }
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o444))?;
            fs::create_dir_all(dst.parent().expect("blob path has a parent"))?;
            let _ = fs::remove_file(&dst);
            fs::rename(&tmp, &dst)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                report.copied += 1;
                report.copied_bytes += blob.size;
                on_copy(&blob.sha256, blob.size);
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                report.failed.push((blob.sha256.clone(), format!("{e:#}")));
            }
        }
    }
    report.only_in_archive = blobs
        .iter()
        .filter(|b| !store.contains(&b.sha256) && archive.blob_path(&b.sha256).is_file())
        .count();

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
    db.lock()
        .expect("database lock")
        .conn()
        .execute("VACUUM INTO ?1", [tmp.to_string_lossy().as_ref()])
        .context("copying the database to the archive")?;
    fs::rename(&tmp, archive.db_path())?;
    Ok(report)
}
