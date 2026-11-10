//! The database beside the store. It records what each stored file is, which
//! repository revisions are archived, and every app file dedupd knows about.

use crate::hub::{Snapshot, TreeEntry};
use crate::now;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

const SCHEMA: &str = r#"
CREATE TABLE blobs (
    sha256    TEXT PRIMARY KEY,
    size      INTEGER NOT NULL,
    git_oid   TEXT,
    added_at  INTEGER NOT NULL,
    pinned    INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX blobs_git_oid ON blobs(git_oid);

CREATE TABLE repos (
    id        INTEGER PRIMARY KEY,
    provider  TEXT NOT NULL,
    name      TEXT NOT NULL,
    UNIQUE (provider, name)
);

-- One archived revision: the provider's own answers, kept verbatim so they
-- can be served again when the provider cannot be asked.
CREATE TABLE revisions (
    id          INTEGER PRIMARY KEY,
    repo_id     INTEGER NOT NULL REFERENCES repos(id),
    commit_sha  TEXT NOT NULL,
    fetched_at  INTEGER NOT NULL,
    info_json   TEXT NOT NULL,
    tree_json   TEXT NOT NULL,
    UNIQUE (repo_id, commit_sha)
);

CREATE TABLE revision_files (
    revision_id  INTEGER NOT NULL REFERENCES revisions(id),
    path         TEXT NOT NULL,
    size         INTEGER NOT NULL,
    git_oid      TEXT NOT NULL,
    lfs_sha256   TEXT,
    PRIMARY KEY (revision_id, path)
);

CREATE TABLE refs (
    repo_id       INTEGER NOT NULL REFERENCES repos(id),
    name          TEXT NOT NULL,
    commit_sha    TEXT NOT NULL,
    confirmed_at  INTEGER NOT NULL,
    PRIMARY KEY (repo_id, name)
);

-- A file in an app's folder that holds the same bytes as a stored blob.
CREATE TABLE placements (
    id               INTEGER PRIMARY KEY,
    app              TEXT NOT NULL,
    path             TEXT NOT NULL UNIQUE,
    repo_id          INTEGER NOT NULL REFERENCES repos(id),
    rel_path         TEXT NOT NULL,
    sha256           TEXT NOT NULL REFERENCES blobs(sha256),
    size             INTEGER NOT NULL,
    mtime_ns         INTEGER NOT NULL,
    origin           TEXT NOT NULL,
    verify_state     TEXT NOT NULL,
    verified_commit  TEXT,
    recorded_at      INTEGER NOT NULL
);
"#;

pub struct Db {
    conn: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementRow {
    pub sha256: String,
    pub git_oid: Option<String>,
    pub size: u64,
    pub mtime_ns: i64,
    pub verify_state: String,
}

pub struct NewPlacement<'a> {
    pub app: &'a str,
    pub path: &'a str,
    pub repo_id: i64,
    pub rel_path: &'a str,
    pub sha256: &'a str,
    pub size: u64,
    pub mtime_ns: i64,
    pub origin: &'a str,
    pub verify_state: &'a str,
    pub verified_commit: Option<&'a str>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        match version {
            0 => {
                conn.execute_batch(&format!("BEGIN; {SCHEMA} PRAGMA user_version = 1; COMMIT;"))?;
            }
            1 => {}
            other => anyhow::bail!(
                "database schema version {other} is newer than this build of dedupd understands"
            ),
        }
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn upsert_repo(&self, provider: &str, name: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT OR IGNORE INTO repos (provider, name) VALUES (?1, ?2)",
            params![provider, name],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM repos WHERE provider = ?1 AND name = ?2",
            params![provider, name],
            |r| r.get(0),
        )?)
    }

    /// Archive a revision and point `ref_name` at it.
    pub fn record_snapshot(&mut self, repo_id: i64, ref_name: &str, snap: &Snapshot) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO revisions (repo_id, commit_sha, fetched_at, info_json, tree_json)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (repo_id, commit_sha) DO UPDATE SET
                 fetched_at = excluded.fetched_at,
                 info_json  = excluded.info_json,
                 tree_json  = excluded.tree_json",
            params![repo_id, snap.commit, now(), snap.info_json, snap.tree_json],
        )?;
        let rev_id: i64 = tx.query_row(
            "SELECT id FROM revisions WHERE repo_id = ?1 AND commit_sha = ?2",
            params![repo_id, snap.commit],
            |r| r.get(0),
        )?;
        tx.execute(
            "DELETE FROM revision_files WHERE revision_id = ?1",
            [rev_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO revision_files (revision_id, path, size, git_oid, lfs_sha256)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for TreeEntry {
                path,
                size,
                git_oid,
                lfs_sha256,
            } in &snap.entries
            {
                stmt.execute(params![rev_id, path, *size as i64, git_oid, lfs_sha256])?;
            }
        }
        tx.execute(
            "INSERT INTO refs (repo_id, name, commit_sha, confirmed_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (repo_id, name) DO UPDATE SET
                 commit_sha = excluded.commit_sha, confirmed_at = excluded.confirmed_at",
            params![repo_id, ref_name, snap.commit, now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_blob(&self, sha256: &str, size: u64, git_oid: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO blobs (sha256, size, git_oid, added_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (sha256) DO UPDATE SET git_oid = COALESCE(excluded.git_oid, blobs.git_oid)",
            params![sha256, size as i64, git_oid, now()],
        )?;
        Ok(())
    }

    pub fn placement(&self, path: &str) -> Result<Option<PlacementRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT p.sha256, b.git_oid, p.size, p.mtime_ns, p.verify_state
                 FROM placements p JOIN blobs b ON b.sha256 = p.sha256
                 WHERE p.path = ?1",
                [path],
                |r| {
                    Ok(PlacementRow {
                        sha256: r.get(0)?,
                        git_oid: r.get(1)?,
                        size: r.get::<_, i64>(2)? as u64,
                        mtime_ns: r.get(3)?,
                        verify_state: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn upsert_placement(&self, p: &NewPlacement<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO placements
                 (app, path, repo_id, rel_path, sha256, size, mtime_ns, origin,
                  verify_state, verified_commit, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (path) DO UPDATE SET
                 app = excluded.app, repo_id = excluded.repo_id, rel_path = excluded.rel_path,
                 sha256 = excluded.sha256, size = excluded.size, mtime_ns = excluded.mtime_ns,
                 verify_state = excluded.verify_state, verified_commit = excluded.verified_commit,
                 recorded_at = excluded.recorded_at",
            params![
                p.app,
                p.path,
                p.repo_id,
                p.rel_path,
                p.sha256,
                p.size as i64,
                p.mtime_ns,
                p.origin,
                p.verify_state,
                p.verified_commit,
                now()
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_created_once_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.sqlite");
        {
            let db = Db::open(&path).unwrap();
            let id = db.upsert_repo("hf", "org/repo").unwrap();
            assert_eq!(db.upsert_repo("hf", "org/repo").unwrap(), id);
        }
        let db = Db::open(&path).unwrap();
        let n: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM repos", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn blob_git_oid_is_kept_when_a_later_upsert_has_none() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_blob("aa", 3, Some("oid")).unwrap();
        db.upsert_blob("aa", 3, None).unwrap();
        let oid: Option<String> = db
            .conn()
            .query_row("SELECT git_oid FROM blobs WHERE sha256 = 'aa'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(oid.as_deref(), Some("oid"));
    }
}
