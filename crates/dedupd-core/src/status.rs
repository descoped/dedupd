//! The totals: what the store holds, what each app holds, and each model.

use crate::db::Db;
use crate::store::Store;
use anyhow::Result;
use dedupd_api::daemon::{AppUsage, Repository};

#[derive(Debug, serde::Serialize)]
pub struct Status {
    pub store_files: u64,
    pub store_bytes: u64,
    /// Bytes of app files that share their disk blocks with the store: what
    /// the apps would use on top of the store if each kept its own copy.
    pub shared_bytes: u64,
    pub apps: Vec<AppUsage>,
    pub repos: Vec<Repository>,
}

/// What the store and the apps hold, in totals.
pub fn status(db: &Db, store: &Store) -> Result<Status> {
    let (store_files, store_bytes) = store_totals(db, store)?;
    Ok(Status {
        store_files,
        store_bytes,
        shared_bytes: db.shared_bytes()?,
        apps: db.app_usage()?,
        repos: db.repositories()?,
    })
}

/// The files the store holds on the internal disk, and their size. A file
/// that is only on the archive disk (offloaded, or of an archived model) is
/// known to the database but takes no room here.
pub fn store_totals(db: &Db, store: &Store) -> Result<(u64, u64)> {
    Ok(store_totals_of(&db.all_blobs()?, store))
}

/// The same count for files already read from the database, for a caller
/// that looks at the disk after the database's lock is given back.
pub fn store_totals_of(blobs: &[crate::db::BlobRow], store: &Store) -> (u64, u64) {
    let here: Vec<u64> = blobs
        .iter()
        .filter(|b| store.contains(&b.sha256))
        .map(|b| b.size)
        .collect();
    (here.len() as u64, here.iter().sum())
}
