//! The totals: what the store holds, what each app holds, and each model.

use crate::db::Db;
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

pub fn status(db: &Db) -> Result<Status> {
    let (store_files, store_bytes) = db.store_totals()?;
    Ok(Status {
        store_files,
        store_bytes,
        shared_bytes: db.shared_bytes()?,
        apps: db.app_usage()?,
        repos: db.repositories()?,
    })
}
