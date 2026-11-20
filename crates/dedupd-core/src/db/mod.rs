//! The database beside the store. It records what each stored file is, which
//! repository revisions are archived, and every app file dedupd knows about.

use crate::hub::{Snapshot, TreeEntry, is_commit, ref_name};
use crate::now;
use crate::store::Fingerprint;
use anyhow::{Context, Result};
use dedupd_api::app::App;
use dedupd_api::jobs::JobState;
use dedupd_api::library::{Origin, VerifyState};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub mod library;

/// `repos.provider` for Hugging Face repositories.
pub const PROVIDER_HF: &str = "hf";
/// What `repos.upstream` holds once the provider was asked: it has the
/// model, or it answered that it does not.
pub const UPSTREAM_OK: &str = "ok";
pub const UPSTREAM_GONE: &str = "gone";

/// A size or a count on its way into the database, which keeps whole
/// numbers as `i64`. No file is 2^63 bytes; one that claimed to be is kept
/// as the largest number there is, not as a negative one.
pub(super) fn stored(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// A size or a count on its way out: a negative number, which nothing
/// here writes, reads as 0 and never as a huge size.
pub(super) fn count(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// `COALESCE(sum(<column> = '<state>'), 0)` for each verify state, in the
/// order verified, mismatch, not upstream, unchecked: the words come from
/// the enum, so a spelling changes in one place.
pub(super) fn verify_counts(column: &str) -> String {
    [
        VerifyState::Verified,
        VerifyState::Mismatch,
        VerifyState::NotUpstream,
        VerifyState::Unchecked,
    ]
    .map(|state| format!("COALESCE(sum({column} = '{}'), 0)", state.as_str()))
    .join(",\n")
}
/// `repos.provider` for models from an Ollama registry other than Hugging
/// Face's.
pub const PROVIDER_OLLAMA: &str = "ollama";

/// `placements.origin` for a file the app had before dedupd knew it.
pub const ORIGIN_ADOPTED: &str = Origin::Adopted.as_str();
/// `placements.origin` for a file dedupd created as a clone.
pub const ORIGIN_PLACED: &str = Origin::Placed.as_str();

const BLOB_COLS: &str = "b.sha256, b.size, b.git_oid";
/// No app file holds this content.
const UNPLACED: &str = "NOT EXISTS (SELECT 1 FROM placements p WHERE p.sha256 = b.sha256)";

/// An app's short name in a column, read as the app.
pub(crate) fn app_column(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<App> {
    let name: String = r.get(i)?;
    name.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Lock the shared database handle.
///
/// A panic on another thread while it held the lock does not stop every
/// later caller: the lock is taken all the same. That is safe because `Db`
/// has no state of its own that a panic can leave half changed; a
/// transaction that was open is rolled back when it is dropped. The panic
/// itself is in the log.
pub fn lock(db: &Mutex<Db>) -> MutexGuard<'_, Db> {
    db.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The schema's number, kept in the database as `user_version`. There are
/// no migrations: a database with another number was made by another build
/// and is refused. Change the number whenever the schema changes.
pub const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = r#"
CREATE TABLE blobs (
    sha256    TEXT PRIMARY KEY,
    size      INTEGER NOT NULL,
    git_oid   TEXT,
    added_at  INTEGER NOT NULL
);
CREATE INDEX blobs_git_oid ON blobs(git_oid);

CREATE TABLE repos (
    id           INTEGER PRIMARY KEY,
    provider     TEXT NOT NULL,
    name         TEXT NOT NULL,
    -- What the provider said when it was last asked about the repository:
    -- 'ok', or 'gone' when it no longer has it or will not show it.
    upstream     TEXT,
    checked_at   INTEGER,
    -- When the owner moved the model out of warm storage: it is in no app,
    -- and its files are on the archive disk only. NULL for a model that is
    -- not archived.
    archived_at  INTEGER,
    UNIQUE (provider, name)
);

-- One kept revision: the provider's own answers, verbatim, so they can be
-- served again when the provider cannot be asked.
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

-- A file in an app's folder that holds the bytes of a stored blob, or was
-- made from them.
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
    recorded_at      INTEGER NOT NULL,
    -- 1 when the app file shares its disk blocks with the store file (one
    -- was cloned from the other); 0 when it is a separate copy of the same
    -- bytes.
    shared           INTEGER NOT NULL DEFAULT 1,
    -- Set for an app file that is not the stored file's bytes but made from
    -- them: a clone with a part rewritten, so a sha256 of its own. `sha256`
    -- still names the stored file it was made from and shares its disk
    -- blocks with; `method` says how it was made, as the app's profile
    -- names it.
    method           TEXT,
    derived_sha256   TEXT
);

-- The stored files a repository is known to hold, whatever the provider
-- lists: every file placed in an app for it, adopted or put there by
-- dedupd. A model taken out of every app keeps its files through this until
-- the owner forgets it; an adopted file the provider does not list (it
-- could not be asked, the file is not upstream, or it is of another
-- revision) would otherwise look unused.
CREATE TABLE repo_files (
    repo_id  INTEGER NOT NULL REFERENCES repos(id),
    sha256   TEXT NOT NULL REFERENCES blobs(sha256),
    PRIMARY KEY (repo_id, sha256)
);
CREATE INDEX repo_files_sha256 ON repo_files(sha256);

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

-- The daemon's download queue. A job is kept whole as JSON; `state` is
-- repeated beside it so jobs can be picked without reading every one. A
-- number is never used twice, so a removed job cannot be mistaken for a
-- later one.
CREATE TABLE downloads (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    state       TEXT NOT NULL,
    job_json    TEXT NOT NULL,
    updated_at  INTEGER NOT NULL
);
"#;

/// A repository the database does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotInLibrary(pub String);

impl std::fmt::Display for NotInLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} is not in the library", self.0)
    }
}

impl std::error::Error for NotInLibrary {}

/// The database of one home: what the provider listed, what the store
/// holds, and every app file dedupd knows. All SQL is in this module; the
/// connection is private to it.
pub struct Db {
    conn: Connection,
}

/// The condition on `blobs b` that a repository matching `repo` (the rest of
/// a comparison with a repository id) refers to the file, through a revision,
/// an Ollama manifest, or a file it was known to hold.
fn used_by(repo: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM revision_files f JOIN revisions v ON v.id = f.revision_id
                  WHERE v.repo_id {repo}
                    AND (f.lfs_sha256 = b.sha256
                         OR (f.lfs_sha256 IS NULL AND f.git_oid = b.git_oid)))
          OR EXISTS (SELECT 1 FROM ollama_manifests m
                     WHERE m.repo_id {repo} AND instr(m.body, b.sha256) > 0)
          OR EXISTS (SELECT 1 FROM repo_files h
                     WHERE h.repo_id {repo} AND h.sha256 = b.sha256))"
    )
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
    /// The file's own sha256 when it was derived from the stored file
    /// rather than cloned as it is.
    pub derived_sha256: Option<String>,
}

/// One app file of a model, as the library shows it.
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

/// An app file to record: where it is, which stored file it is, and how
/// it came to be there.
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
    /// For a file made from the stored file by a method of the app's
    /// profile: the method and the file's own sha256.
    pub derived: Option<(&'a str, &'a str)>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn).with_context(|| format!("opening {}", path.display()))
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Create the schema in an empty database. One that holds another
    /// schema is refused: there are no migrations, so it has to be removed.
    fn init(conn: Connection) -> Result<Self> {
        // First, so that everything below waits its turn.
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        // A new database is switched to WAL by whoever opens it first, and
        // for that switch SQLite does not wait for another connection that
        // is making the same one: it answers "locked" at once, whatever
        // the busy timeout. So that one step is tried again here.
        let waited = std::time::Instant::now();
        loop {
            match conn.pragma_update(None, "journal_mode", "WAL") {
                Ok(()) => break,
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if matches!(
                        e.code,
                        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                    ) && waited.elapsed() < std::time::Duration::from_secs(30) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => return Err(e.into()),
            }
        }
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // The look and the creation are one step, under the write lock: two
        // processes that open a new home at the same moment (the daemon and
        // a command) take turns, and the second finds the schema made.
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let made = (|| -> Result<i64> {
            let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            let tables: i64 = conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )?;
            if version == 0 && tables == 0 {
                conn.execute_batch(&format!("{SCHEMA} PRAGMA user_version = {SCHEMA_VERSION};"))?;
                return Ok(SCHEMA_VERSION);
            }
            Ok(version)
        })();
        let version = match made {
            Ok(version) => {
                conn.execute_batch("COMMIT")?;
                version
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        };
        if version != SCHEMA_VERSION {
            anyhow::bail!(
                "the database was made by another build of dedupd (schema {version}, this build uses \
                 {SCHEMA_VERSION}). dedupd does not migrate a database: remove it and its -wal and \
                 -shm files, then let dedupd adopt the models again"
            );
        }
        Ok(Self { conn })
    }

    /// Add a job to the download queue and return its id.
    ///
    /// The row names no model until the queue saves the job into it, and
    /// `has_unfinished_job` fails on such a row. The two never meet: the
    /// queue adds a job and a move starts under one lock of the daemon
    /// (`Server::starting`), and the command line adds no jobs.
    pub fn insert_download(&self, state: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO downloads (state, job_json, updated_at) VALUES (?1, '{}', ?2)",
            params![state, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Save a job of the download queue: its state as a word, and the
    /// whole job as the queue wrote it.
    pub fn save_download(&self, id: i64, state: &str, job_json: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE downloads SET state = ?2, job_json = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, state, job_json, now()],
        )?;
        Ok(())
    }

    /// Every job of the download queue as it was last saved, oldest first,
    /// with its number.
    pub fn downloads(&self) -> Result<Vec<(i64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, job_json FROM downloads ORDER BY id")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Whether a job of the download queue that is not finished (queued,
    /// running, paused, or waiting for the owner) is for `repo`. Such a job
    /// can still fetch or place the model's files, so the model is not
    /// archived or restored meanwhile.
    pub fn has_unfinished_job(&self, repo: &str) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare("SELECT job_json FROM downloads WHERE state NOT IN (?1, ?2)")?;
        let finished = [JobState::Done.as_str(), JobState::Cancelled.as_str()];
        let rows = stmt.query_map(finished, |r| r.get::<_, String>(0))?;
        // Only the model's name is read, so a row another build saved
        // still answers. A row with no name (not JSON, or one just made
        // and not yet saved by the queue) is an error: the caller is about
        // to move a model's files and must not take "cannot tell" for no.
        #[derive(serde::Deserialize)]
        struct Named {
            repo: String,
        }
        for json in rows {
            let job: Named = serde_json::from_str(&json?)
                .context("a job in the download queue cannot be read; try again in a moment")?;
            if job.repo == repo {
                return Ok(true);
            }
        }
        Ok(false)
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
            params![
                provider,
                repo,
                if gone { UPSTREAM_GONE } else { UPSTREAM_OK },
                now()
            ],
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
                stmt.execute(params![rev_id, path, stored(*size), git_oid, lfs_sha256])?;
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
            params![sha256, stored(size), git_oid, now()],
        )?;
        Ok(())
    }

    /// Record a stored file only when a repository refers to it: through a
    /// revision, an Ollama manifest or a file it held. Returns whether it
    /// was recorded. A file nothing refers to stays out of the records, so
    /// it never shows up as something `gc` could remove.
    pub fn record_blob_if_used(
        &self,
        sha256: &str,
        size: u64,
        git_oid: Option<&str>,
    ) -> Result<bool> {
        let added = self.conn.execute(
            &format!(
                "INSERT INTO blobs (sha256, size, git_oid, added_at)
                 SELECT b.sha256, b.size, b.git_oid, b.added_at
                 FROM (SELECT ?1 AS sha256, ?2 AS size, ?3 AS git_oid, ?4 AS added_at) b
                 WHERE {}
                 ON CONFLICT (sha256) DO NOTHING",
                used_by("IS NOT NULL")
            ),
            params![sha256, stored(size), git_oid, now()],
        )?;
        Ok(added > 0)
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
                        size: count(r.get(2)?),
                        mtime_ns: r.get(3)?,
                        verify_state: r.get(4)?,
                        shared: r.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    /// Record an app file, and that its repository holds the stored file.
    pub fn upsert_placement(&self, p: &NewPlacement<'_>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO placements
                 (app, path, repo_id, rel_path, sha256, size, mtime_ns, origin,
                  verify_state, verified_commit, recorded_at, shared, method, derived_sha256)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT (path) DO UPDATE SET
                 app = excluded.app, repo_id = excluded.repo_id, rel_path = excluded.rel_path,
                 sha256 = excluded.sha256, size = excluded.size, mtime_ns = excluded.mtime_ns,
                 verify_state = excluded.verify_state, verified_commit = excluded.verified_commit,
                 recorded_at = excluded.recorded_at, shared = excluded.shared,
                 origin = excluded.origin, method = excluded.method,
                 derived_sha256 = excluded.derived_sha256",
            params![
                p.app,
                p.path,
                p.repo_id,
                p.rel_path,
                p.sha256,
                stored(p.size),
                p.mtime_ns,
                p.origin,
                p.verify_state,
                p.verified_commit,
                now(),
                p.shared,
                p.derived.map(|d| d.0),
                p.derived.map(|d| d.1)
            ],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO repo_files (repo_id, sha256) VALUES (?1, ?2)",
            params![p.repo_id, p.sha256],
        )?;
        tx.commit()?;
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

    /// The id of a Hugging Face repository the database knows; otherwise
    /// the error [`NotInLibrary`].
    pub fn known_repo(&self, repo: &str) -> Result<i64> {
        self.repo_id(PROVIDER_HF, repo)?
            .ok_or_else(|| NotInLibrary(repo.to_string()).into())
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
                    size: count(r.get(1)?),
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

    /// Stored files that no app file, and no repository through a revision,
    /// an Ollama manifest or a file it held, refers to.
    pub fn unreferenced_blobs(&self) -> Result<Vec<BlobRow>> {
        self.blobs_where(&format!("{UNPLACED} AND NOT {}", used_by("IS NOT NULL")))
    }

    /// Whether the stored file is one of `unreferenced_blobs` now.
    pub fn is_unreferenced(&self, sha256: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM blobs b
                 WHERE b.sha256 = ?1 AND {UNPLACED} AND NOT {})",
                used_by("IS NOT NULL")
            ),
            [sha256],
            |r| r.get(0),
        )?)
    }

    /// The stored files a repository's archived revisions, Ollama manifests
    /// and app files refer to, now or before.
    pub fn repo_blobs(&self, repo_id: i64) -> Result<Vec<BlobRow>> {
        self.blobs_where(&used_by(&format!("= {repo_id}")))
    }

    /// Of those, the ones another repository refers to as well, unless that
    /// repository is archived itself.
    pub fn repo_blobs_used_elsewhere(&self, repo_id: i64) -> Result<Vec<BlobRow>> {
        self.blobs_where(&format!(
            "{} AND {}",
            used_by(&format!("= {repo_id}")),
            used_by(&format!(
                "IN (SELECT id FROM repos WHERE id != {repo_id} AND archived_at IS NULL)"
            ))
        ))
    }

    /// Mark a repository as archived at `at`, or as not archived.
    pub fn set_archived(&self, repo_id: i64, at: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE repos SET archived_at = ?2 WHERE id = ?1",
            params![repo_id, at],
        )?;
        Ok(())
    }

    /// When the model was archived; `None` when it is not. Fails for a
    /// repository id that is not in the database.
    pub fn archived_at(&self, repo_id: i64) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT archived_at FROM repos WHERE id = ?1",
            [repo_id],
            |r| r.get(0),
        )?)
    }

    /// Drop a stored file's record. Fails while a placement still refers
    /// to it; the callers check first.
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

    /// Whether this repository is known to hold the stored file: as one of
    /// its files in an app, or as a layer of an Ollama manifest kept for it.
    pub fn repo_refers_to(&self, repo_id: i64, sha256: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM repo_files WHERE repo_id = ?1 AND sha256 = ?2)
                 OR EXISTS (SELECT 1 FROM ollama_manifests
                            WHERE repo_id = ?1 AND instr(body, 'sha256:' || ?2) > 0)",
            params![repo_id, sha256],
            |r| r.get(0),
        )?)
    }

    /// Whether `body` is a manifest kept for this repository, under any
    /// tag: one dedupd fetched, or one it found in Ollama's folder.
    pub fn keeps_manifest(&self, repo_id: i64, body: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM ollama_manifests WHERE repo_id = ?1 AND body = ?2)",
            params![repo_id, body],
            |r| r.get(0),
        )?)
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
            "SELECT p.app, p.path, r.name, p.rel_path, p.sha256, p.size, p.mtime_ns, p.shared,
                    p.derived_sha256
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
                    size: count(r.get(5)?),
                    mtime_ns: r.get(6)?,
                    shared: r.get(7)?,
                    derived_sha256: r.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The sha256 of every app file that was derived from a stored file.
    /// Such a file is dedupd's own work: it is not new content to adopt.
    pub fn derived_files(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT derived_sha256 FROM placements WHERE derived_sha256 IS NOT NULL")?;
        Ok(stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Drop a repository's archive records: its revisions, refs, Ollama
    /// manifests and the files it held, then the repository itself. Stored
    /// files are not touched.
    pub fn delete_repo(&mut self, repo_id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        for sql in [
            "DELETE FROM revision_files
             WHERE revision_id IN (SELECT id FROM revisions WHERE repo_id = ?1)",
            "DELETE FROM revisions WHERE repo_id = ?1",
            "DELETE FROM refs WHERE repo_id = ?1",
            "DELETE FROM ollama_manifests WHERE repo_id = ?1",
            "DELETE FROM repo_files WHERE repo_id = ?1",
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
        // The path as text: one that is not UTF-8 is refused, not copied
        // to a name that looks like it.
        let path = path
            .to_str()
            .with_context(|| format!("{} is not a name SQLite can take", path.display()))?;
        self.conn.execute("VACUUM INTO ?1", [path])?;
        Ok(())
    }

    /// Drop the record of the app file at `path`; nothing when there is none.
    pub fn delete_placement(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM placements WHERE path = ?1", [path])?;
        Ok(())
    }

    /// Note that the app file at `path` is now a clone of the stored file
    /// (reclaim replaced it), with the clone's time.
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
        size: count(r.get(1)?),
        git_oid: r.get(2)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_database_opened_from_many_sides_at_once_is_made_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedupd.sqlite");
        let gate = std::sync::Arc::new(std::sync::Barrier::new(8));
        let opening: Vec<_> = (0..8)
            .map(|_| {
                let (path, gate) = (path.clone(), gate.clone());
                std::thread::spawn(move || {
                    gate.wait();
                    Db::open(&path).map(|_| ())
                })
            })
            .collect();
        for one in opening {
            one.join().unwrap().unwrap();
        }
    }

    /// A model whose kept Ollama manifest names one stored layer.
    fn with_manifest(db: &Db, name: &str, layer: &str) -> i64 {
        let id = db.upsert_repo(PROVIDER_HF, name).unwrap();
        let body = format!(r#"{{"layers":[{{"digest":"sha256:{layer}"}}]}}"#);
        db.save_manifest(id, "latest", "ollama/1", "application/json", &body)
            .unwrap();
        id
    }

    #[test]
    fn the_manifest_of_the_one_asking_comes_first_then_the_newest() {
        let db = Db::open_in_memory().unwrap();
        let id = db.upsert_repo(PROVIDER_HF, "org/repo").unwrap();
        for (agent, body) in [("ollama/1", "one"), ("ollama/2", "two")] {
            db.save_manifest(id, "latest", agent, "application/json", body)
                .unwrap();
        }
        let body = |agent| db.manifest(id, "latest", agent).unwrap().unwrap().1;
        assert_eq!(body(Some("ollama/1")), "one");
        assert_eq!(body(Some("ollama/2")), "two");
        // An agent nothing was kept for, and no agent: the newest.
        assert_eq!(body(Some("ollama/3")), "two");
        assert_eq!(body(None), "two");
        assert!(db.manifest(id, "other", None).unwrap().is_none());
    }

    #[test]
    fn a_file_is_recorded_only_when_a_model_refers_to_it() {
        let db = Db::open_in_memory().unwrap();
        let (used, unused) = ("a".repeat(64), "b".repeat(64));
        let id = with_manifest(&db, "org/repo", &used);
        assert!(!db.record_blob_if_used(&unused, 3, None).unwrap());
        assert!(db.record_blob_if_used(&used, 3, None).unwrap());
        // A file a model names is no orphan; `gc` asks this again before
        // it removes one.
        assert!(!db.is_unreferenced(&used).unwrap());
        db.upsert_blob(&unused, 3, None).unwrap();
        assert!(db.is_unreferenced(&unused).unwrap());
        db.delete_blob(&unused).unwrap();
        // Recorded once.
        assert!(!db.record_blob_if_used(&used, 3, None).unwrap());
        let known: Vec<String> = db
            .all_blobs()
            .unwrap()
            .into_iter()
            .map(|b| b.sha256)
            .collect();
        assert_eq!(known, [used]);
        // What the library counts as stored is what archive and gc act on,
        // a layer only a kept manifest names included.
        assert_eq!(db.stored_of(id).unwrap(), (1, 3));
        assert_eq!(db.repo_blobs(id).unwrap().len(), 1);
    }

    #[test]
    fn a_file_another_model_uses_keeps_it_unless_that_model_is_archived() {
        let db = Db::open_in_memory().unwrap();
        let layer = "c".repeat(64);
        let one = with_manifest(&db, "org/one", &layer);
        let other = with_manifest(&db, "org/other", &layer);
        db.upsert_blob(&layer, 3, None).unwrap();
        assert_eq!(db.repo_blobs_used_elsewhere(one).unwrap().len(), 1);
        db.set_archived(other, Some(1)).unwrap();
        assert!(db.repo_blobs_used_elsewhere(one).unwrap().is_empty());
        assert_eq!(db.archived_at(other).unwrap(), Some(1));
        assert_eq!(db.archived_at(one).unwrap(), None);
    }

    #[test]
    fn a_job_that_cannot_be_read_is_not_taken_for_no_job() {
        let db = Db::open_in_memory().unwrap();
        assert!(!db.has_unfinished_job("org/repo").unwrap());
        let id = db.insert_download(JobState::Queued.as_str()).unwrap();
        // Just made, not yet saved by the queue: it names no model.
        assert!(db.has_unfinished_job("org/repo").is_err());
        // A row of another build still answers by the model's name.
        db.save_download(id, "queued", r#"{"repo":"org/repo","new_field":1}"#)
            .unwrap();
        assert!(db.has_unfinished_job("org/repo").unwrap());
        assert!(!db.has_unfinished_job("org/other").unwrap());
        db.save_download(id, JobState::Done.as_str(), "not json")
            .unwrap();
        assert!(!db.has_unfinished_job("org/repo").unwrap());
    }

    #[test]
    fn numbers_cross_into_the_database_and_back_by_one_rule() {
        assert_eq!(stored(u64::MAX), i64::MAX);
        assert_eq!((count(-1), count(7)), (0, 7));
        assert!(verify_counts("p.verify_state").contains("p.verify_state = 'not_upstream'"));
    }

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
            .conn
            .query_row("SELECT count(*) FROM repos", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_database_of_another_build_is_refused_not_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.sqlite");
        drop(Db::open(&path).unwrap());
        // The one schema holds everything; nothing is added afterwards.
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        for column in ["shared", "method", "derived_sha256"] {
            conn.prepare(&format!("SELECT {column} FROM placements"))
                .unwrap();
        }
        conn.prepare("SELECT upstream, checked_at, archived_at FROM repos")
            .unwrap();
        conn.prepare("SELECT job_json FROM downloads").unwrap();
        conn.prepare("SELECT body FROM ollama_manifests").unwrap();
        conn.prepare("SELECT repo_id, sha256 FROM repo_files")
            .unwrap();

        for other in [SCHEMA_VERSION + 1, SCHEMA_VERSION + 4] {
            conn.pragma_update(None, "user_version", other).unwrap();
            let refused = format!("{:#}", Db::open(&path).err().unwrap());
            assert!(refused.contains("does not migrate"), "{refused}");
            assert!(refused.contains("d.sqlite"), "{refused}");
        }
        // Tables without a version are someone else's file, not an empty one.
        conn.pragma_update(None, "user_version", 0).unwrap();
        assert!(Db::open(&path).is_err());
        // Nothing was changed by the refusals.
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .unwrap();
        drop(conn);
        Db::open(&path).unwrap();
    }

    #[test]
    fn blob_git_oid_is_kept_when_a_later_upsert_has_none() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_blob("aa", 3, Some("oid")).unwrap();
        db.upsert_blob("aa", 3, None).unwrap();
        let oid: Option<String> = db
            .conn
            .query_row("SELECT git_oid FROM blobs WHERE sha256 = 'aa'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(oid.as_deref(), Some("oid"));
    }
}
