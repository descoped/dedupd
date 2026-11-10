use crate::db::Db;
use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Status {
    pub store_files: u64,
    pub store_bytes: u64,
    /// Bytes of app files that share their disk blocks with the store: what
    /// the apps would use on top of the store if each kept its own copy.
    pub shared_bytes: u64,
    pub apps: Vec<AppStatus>,
    pub repos: Vec<RepoStatus>,
}

#[derive(Debug, Serialize)]
pub struct AppStatus {
    pub app: String,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct RepoStatus {
    pub provider: String,
    pub name: String,
    /// The commit `main` pointed at when the provider was last asked.
    pub commit: Option<String>,
    /// Files of this repository's archived revisions that are in the store.
    pub stored_files: u64,
    pub stored_bytes: u64,
    /// App files that hold this repository's content.
    pub files: u64,
    pub bytes: u64,
    pub verified: u64,
    pub mismatch: u64,
    pub not_upstream: u64,
    pub unchecked: u64,
}

pub fn status(db: &Db) -> Result<Status> {
    let conn = db.conn();
    let (store_files, store_bytes): (i64, i64) = conn.query_row(
        "SELECT count(*), COALESCE(sum(size), 0) FROM blobs",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;

    let shared_bytes: i64 = conn.query_row(
        "SELECT COALESCE(sum(size), 0) FROM placements WHERE shared = 1",
        [],
        |r| r.get(0),
    )?;

    let mut stmt = conn.prepare(
        "SELECT app, count(*), COALESCE(sum(size), 0) FROM placements GROUP BY app ORDER BY app",
    )?;
    let apps = stmt
        .query_map([], |r| {
            Ok(AppStatus {
                app: r.get(0)?,
                files: r.get::<_, i64>(1)? as u64,
                bytes: r.get::<_, i64>(2)? as u64,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut stmt = conn.prepare(
        "SELECT r.provider, r.name,
                (SELECT commit_sha FROM refs WHERE repo_id = r.id AND name = 'main'),
                count(p.id), COALESCE(sum(p.size), 0),
                COALESCE(sum(p.verify_state = 'verified'), 0),
                COALESCE(sum(p.verify_state = 'mismatch'), 0),
                COALESCE(sum(p.verify_state = 'not_upstream'), 0),
                COALESCE(sum(p.verify_state = 'unchecked'), 0),
                r.id
         FROM repos r LEFT JOIN placements p ON p.repo_id = r.id
         GROUP BY r.id ORDER BY r.provider, r.name",
    )?;
    // Small files are known to the Hub by their git blob id, weights by sha256.
    let mut stored = conn.prepare(
        "SELECT count(*), COALESCE(sum(size), 0) FROM blobs WHERE sha256 IN (
             SELECT COALESCE(f.lfs_sha256,
                             (SELECT sha256 FROM blobs WHERE git_oid = f.git_oid LIMIT 1))
             FROM revision_files f JOIN revisions v ON v.id = f.revision_id
             WHERE v.repo_id = ?1)",
    )?;
    let repos = stmt
        .query_map([], |r| {
            let n = |i: usize| r.get::<_, i64>(i).map(|v| v as u64);
            let (stored_files, stored_bytes) = stored.query_row([r.get::<_, i64>(9)?], |s| {
                Ok((s.get::<_, i64>(0)? as u64, s.get::<_, i64>(1)? as u64))
            })?;
            Ok(RepoStatus {
                provider: r.get(0)?,
                name: r.get(1)?,
                commit: r.get(2)?,
                stored_files,
                stored_bytes,
                files: n(3)?,
                bytes: n(4)?,
                verified: n(5)?,
                mismatch: n(6)?,
                not_upstream: n(7)?,
                unchecked: n(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(Status {
        store_files: store_files as u64,
        store_bytes: store_bytes as u64,
        shared_bytes: shared_bytes as u64,
        apps,
        repos,
    })
}
