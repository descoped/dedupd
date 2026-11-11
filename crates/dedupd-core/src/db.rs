//! The database beside the store. It records what each stored file is, which
//! repository revisions are archived, and every app file dedupd knows about.

use crate::hub::{Snapshot, TreeEntry};
use crate::now;
use crate::store::Fingerprint;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// `repos.provider` for Hugging Face repositories.
pub const PROVIDER_HF: &str = "hf";
/// `repos.provider` for models from an Ollama registry other than Hugging
/// Face's.
pub const PROVIDER_OLLAMA: &str = "ollama";

/// `placements.origin` for a file the app had before dedupd knew it.
pub const ORIGIN_ADOPTED: &str = "adopted";
/// `placements.origin` for a file dedupd created as a clone.
pub const ORIGIN_PLACED: &str = "placed";

const BLOB_COLS: &str = "b.sha256, b.size, b.git_oid";
/// No app file holds this content.
const UNPLACED: &str = "NOT EXISTS (SELECT 1 FROM placements p WHERE p.sha256 = b.sha256)";

/// Lock the shared database handle.
pub fn lock(db: &Mutex<Db>) -> MutexGuard<'_, Db> {
    db.lock().expect("database lock")
}

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

/// Added in schema version 2.
const SCHEMA_V2: &str = r#"
-- 1 when the app file shares its disk blocks with the store file (one was
-- cloned from the other); 0 when it is a separate copy of the same bytes.
ALTER TABLE placements ADD COLUMN shared INTEGER NOT NULL DEFAULT 1;

-- The manifest the provider's Ollama registry generated for a tag. The answer
-- depends on who asks, so the asking User-Agent is part of the key.
CREATE TABLE ollama_manifests (
    id          INTEGER PRIMARY KEY,
    repo_id     INTEGER NOT NULL REFERENCES repos(id),
    tag         TEXT NOT NULL,
    user_agent  TEXT NOT NULL,
    media_type  TEXT NOT NULL,
    body        TEXT NOT NULL,
    fetched_at  INTEGER NOT NULL,
    UNIQUE (repo_id, tag, user_agent)
);
"#;

/// Added in schema version 3.
const SCHEMA_V3: &str = r#"
-- The daemon's download queue. A job is kept whole as JSON; `state` is
-- repeated beside it so jobs can be picked without reading every one.
CREATE TABLE downloads (
    id          INTEGER PRIMARY KEY,
    state       TEXT NOT NULL,
    job_json    TEXT NOT NULL,
    updated_at  INTEGER NOT NULL
);

-- What the provider said when it was last asked about the repository:
-- 'ok', or 'gone' when it no longer has it or will not show it.
ALTER TABLE repos ADD COLUMN upstream TEXT;
ALTER TABLE repos ADD COLUMN checked_at INTEGER;
"#;

pub struct Db {
    conn: Connection,
}

/// One stored file as the database knows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BlobRow {
    pub sha256: String,
    pub size: u64,
    pub git_oid: Option<String>,
}

/// An app file recorded in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedFile {
    pub app: String,
    pub path: String,
    pub repo: String,
    pub rel_path: String,
    pub sha256: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub shared: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementRow {
    pub sha256: String,
    pub git_oid: Option<String>,
    pub size: u64,
    pub mtime_ns: i64,
    pub verify_state: String,
    pub shared: bool,
}

impl PlacedFile {
    /// What the file looked like when it was recorded.
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint {
            size: self.size,
            mtime_ns: self.mtime_ns,
        }
    }
}

impl PlacementRow {
    /// What the file looked like when it was recorded.
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint {
            size: self.size,
            mtime_ns: self.mtime_ns,
        }
    }
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
    /// Whether the app file shares disk blocks with the store file.
    pub shared: bool,
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
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 3 {
            anyhow::bail!(
                "database schema version {version} is newer than this build of dedupd understands"
            );
        }
        if version < 1 {
            conn.execute_batch(&format!("BEGIN; {SCHEMA} PRAGMA user_version = 1; COMMIT;"))?;
        }
        if version < 2 {
            conn.execute_batch(&format!(
                "BEGIN; {SCHEMA_V2} PRAGMA user_version = 2; COMMIT;"
            ))?;
        }
        if version < 3 {
            conn.execute_batch(&format!(
                "BEGIN; {SCHEMA_V3} PRAGMA user_version = 3; COMMIT;"
            ))?;
        }
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Add a job to the download queue and return its id.
    pub fn insert_download(&self, state: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO downloads (state, job_json, updated_at) VALUES (?1, '{}', ?2)",
            params![state, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn save_download(&self, id: i64, state: &str, job_json: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE downloads SET state = ?2, job_json = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, state, job_json, now()],
        )?;
        Ok(())
    }

    /// Every job of the download queue as it was last saved, oldest first.
    pub fn downloads(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT job_json FROM downloads ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    pub fn delete_download(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM downloads WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Note what the provider said about `repo` when it was just asked.
    pub fn set_upstream(&self, provider: &str, repo: &str, gone: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE repos SET upstream = ?3, checked_at = ?4 WHERE provider = ?1 AND name = ?2",
            params![provider, repo, if gone { "gone" } else { "ok" }, now()],
        )?;
        Ok(())
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
    pub fn record_snapshot(
        &mut self,
        repo_id: i64,
        ref_name: Option<&str>,
        snap: &Snapshot,
    ) -> Result<()> {
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
        if let Some(ref_name) = ref_name {
            tx.execute(
                "INSERT INTO refs (repo_id, name, commit_sha, confirmed_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (repo_id, name) DO UPDATE SET
                     commit_sha = excluded.commit_sha, confirmed_at = excluded.confirmed_at",
                params![repo_id, ref_name, snap.commit, now()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Archive a revision of `provider`'s `repo` as asked for by `revision`
    /// (a branch, tag or commit). A branch or tag name is recorded as a ref;
    /// a commit id is not one. Returns the repository's id.
    pub fn archive_snapshot(
        &mut self,
        provider: &str,
        repo: &str,
        revision: &str,
        snap: &Snapshot,
    ) -> Result<i64> {
        let repo_id = self.upsert_repo(provider, repo)?;
        self.record_snapshot(repo_id, ref_name(revision), snap)?;
        Ok(repo_id)
    }

    /// The archived revision of `provider`'s `repo`, with the repository's
    /// id. `revision` is as for `archived_snapshot`.
    pub fn archived(
        &self,
        provider: &str,
        repo: &str,
        revision: &str,
    ) -> Result<Option<(i64, Snapshot)>> {
        Ok(match self.repo_id(provider, repo)? {
            Some(id) => self.archived_snapshot(id, revision)?.map(|s| (id, s)),
            None => None,
        })
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
                "SELECT p.sha256, b.git_oid, p.size, p.mtime_ns, p.verify_state, p.shared
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
                        shared: r.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn upsert_placement(&self, p: &NewPlacement<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO placements
                 (app, path, repo_id, rel_path, sha256, size, mtime_ns, origin,
                  verify_state, verified_commit, recorded_at, shared)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT (path) DO UPDATE SET
                 app = excluded.app, repo_id = excluded.repo_id, rel_path = excluded.rel_path,
                 sha256 = excluded.sha256, size = excluded.size, mtime_ns = excluded.mtime_ns,
                 verify_state = excluded.verify_state, verified_commit = excluded.verified_commit,
                 recorded_at = excluded.recorded_at, shared = excluded.shared,
                 origin = excluded.origin",
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
                now(),
                p.shared
            ],
        )?;
        Ok(())
    }

    /// Point a branch or tag name at a commit.
    pub fn set_ref(&self, repo_id: i64, name: &str, commit: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO refs (repo_id, name, commit_sha, confirmed_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (repo_id, name) DO UPDATE SET
                 commit_sha = excluded.commit_sha, confirmed_at = excluded.confirmed_at",
            params![repo_id, name, commit, now()],
        )?;
        Ok(())
    }

    pub fn repo_id(&self, provider: &str, name: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM repos WHERE provider = ?1 AND name = ?2",
                params![provider, name],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The commit an archived branch or tag name pointed at when last confirmed.
    pub fn ref_commit(&self, repo_id: i64, name: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT commit_sha FROM refs WHERE repo_id = ?1 AND name = ?2",
                params![repo_id, name],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Every archived branch and tag name of a repository, with its commit.
    pub fn refs(&self, repo_id: i64) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, commit_sha FROM refs WHERE repo_id = ?1 ORDER BY name")?;
        Ok(stmt
            .query_map([repo_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// An archived revision. `revision` is a commit id, or a branch or tag
    /// name that was archived earlier.
    pub fn archived_snapshot(&self, repo_id: i64, revision: &str) -> Result<Option<Snapshot>> {
        let commit = if is_commit(revision) {
            revision.to_string()
        } else {
            match self.ref_commit(repo_id, revision)? {
                Some(c) => c,
                None => return Ok(None),
            }
        };
        let row: Option<(i64, String, String)> = self
            .conn
            .query_row(
                "SELECT id, info_json, tree_json FROM revisions
                 WHERE repo_id = ?1 AND commit_sha = ?2",
                params![repo_id, commit],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((rev_id, info_json, tree_json)) = row else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare(
            "SELECT path, size, git_oid, lfs_sha256 FROM revision_files
             WHERE revision_id = ?1 ORDER BY path",
        )?;
        let entries = stmt
            .query_map([rev_id], |r| {
                Ok(TreeEntry {
                    path: r.get(0)?,
                    size: r.get::<_, i64>(1)? as u64,
                    git_oid: r.get(2)?,
                    lfs_sha256: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(Snapshot {
            commit,
            info_json,
            tree_json,
            entries,
        }))
    }

    pub fn blob(&self, sha256: &str) -> Result<Option<BlobRow>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {BLOB_COLS} FROM blobs b WHERE b.sha256 = ?1"),
                [sha256],
                blob_row,
            )
            .optional()?)
    }

    /// The stored file whose git blob id is `oid`, which is how the Hub names
    /// files that are not in LFS.
    pub fn blob_by_git_oid(&self, oid: &str) -> Result<Option<BlobRow>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {BLOB_COLS} FROM blobs b WHERE b.git_oid = ?1 LIMIT 1"),
                [oid],
                blob_row,
            )
            .optional()?)
    }

    /// The stored files matching `condition`, in sha256 order.
    fn blobs_where(&self, condition: &str) -> Result<Vec<BlobRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {BLOB_COLS} FROM blobs b WHERE {condition} ORDER BY b.sha256"
        ))?;
        Ok(stmt
            .query_map([], blob_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn all_blobs(&self) -> Result<Vec<BlobRow>> {
        self.blobs_where("1")
    }

    /// Stored files that no app file holds.
    pub fn unplaced_blobs(&self) -> Result<Vec<BlobRow>> {
        self.blobs_where(UNPLACED)
    }

    /// Stored files that no archived revision, Ollama manifest or app file
    /// refers to.
    pub fn unreferenced_blobs(&self) -> Result<Vec<BlobRow>> {
        self.blobs_where(&format!(
            "{UNPLACED}
               AND NOT EXISTS (SELECT 1 FROM revision_files f
                               WHERE f.lfs_sha256 = b.sha256
                                  OR (f.lfs_sha256 IS NULL AND f.git_oid = b.git_oid))
               AND NOT EXISTS (SELECT 1 FROM ollama_manifests m
                               WHERE instr(m.body, b.sha256) > 0)"
        ))
    }

    pub fn delete_blob(&self, sha256: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM blobs WHERE sha256 = ?1", [sha256])?;
        Ok(())
    }

    /// The stored content for a file the Hub lists, if it is archived.
    pub fn blob_for_entry(&self, entry: &TreeEntry) -> Result<Option<BlobRow>> {
        match &entry.lfs_sha256 {
            Some(sha) => self.blob(sha),
            None => self.blob_by_git_oid(&entry.git_oid),
        }
    }

    pub fn save_manifest(
        &self,
        repo_id: i64,
        tag: &str,
        user_agent: &str,
        media_type: &str,
        body: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO ollama_manifests (repo_id, tag, user_agent, media_type, body, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (repo_id, tag, user_agent) DO UPDATE SET
                 media_type = excluded.media_type, body = excluded.body,
                 fetched_at = excluded.fetched_at",
            params![repo_id, tag, user_agent, media_type, body, now()],
        )?;
        Ok(())
    }

    /// An archived Ollama manifest as `(media type, body)`: the one fetched
    /// with this User-Agent, else the most recent one for the tag.
    pub fn manifest(
        &self,
        repo_id: i64,
        tag: &str,
        user_agent: Option<&str>,
    ) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT media_type, body FROM ollama_manifests
                 WHERE repo_id = ?1 AND tag = ?2
                 ORDER BY (user_agent = ?3) DESC, fetched_at DESC, id DESC LIMIT 1",
                params![repo_id, tag, user_agent.unwrap_or("")],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn placements(&self) -> Result<Vec<PlacedFile>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.app, p.path, r.name, p.rel_path, p.sha256, p.size, p.mtime_ns, p.shared
             FROM placements p JOIN repos r ON r.id = p.repo_id ORDER BY p.path",
        )?;
        Ok(stmt
            .query_map([], |r| {
                Ok(PlacedFile {
                    app: r.get(0)?,
                    path: r.get(1)?,
                    repo: r.get(2)?,
                    rel_path: r.get(3)?,
                    sha256: r.get(4)?,
                    size: r.get::<_, i64>(5)? as u64,
                    mtime_ns: r.get(6)?,
                    shared: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Drop a repository's archive records: its revisions, refs and Ollama
    /// manifests, then the repository itself. Stored files are not touched.
    pub fn delete_repo(&mut self, repo_id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        for sql in [
            "DELETE FROM revision_files
             WHERE revision_id IN (SELECT id FROM revisions WHERE repo_id = ?1)",
            "DELETE FROM revisions WHERE repo_id = ?1",
            "DELETE FROM refs WHERE repo_id = ?1",
            "DELETE FROM ollama_manifests WHERE repo_id = ?1",
            "DELETE FROM repos WHERE id = ?1",
        ] {
            tx.execute(sql, [repo_id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Write a consistent copy of the whole database to `path`, which must
    /// not exist.
    pub fn vacuum_into(&self, path: &Path) -> Result<()> {
        self.conn
            .execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])?;
        Ok(())
    }

    pub fn delete_placement(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM placements WHERE path = ?1", [path])?;
        Ok(())
    }

    pub fn set_placement_shared(&self, path: &str, mtime_ns: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE placements SET shared = 1, mtime_ns = ?2 WHERE path = ?1",
            params![path, mtime_ns],
        )?;
        Ok(())
    }
}

fn blob_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<BlobRow> {
    Ok(BlobRow {
        sha256: r.get(0)?,
        size: r.get::<_, i64>(1)? as u64,
        git_oid: r.get(2)?,
    })
}

/// True for a full 40-character git commit id.
pub fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The branch or tag name in `revision`; `None` when it is a commit id,
/// which is not a ref.
pub fn ref_name(revision: &str) -> Option<&str> {
    (!is_commit(revision)).then_some(revision)
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
