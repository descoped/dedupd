//! The read-only queries behind the library view, the totals and the
//! storage screen.

use super::{Db, PROVIDER_HF, app_column, count, used_by, verify_counts};
use crate::hub::TreeEntry;
use anyhow::Result;
use dedupd_api::app::App;
use dedupd_api::daemon::{AppUsage, Repository};
use dedupd_api::library::{AppPresence, Origin, Placement, VerifyState};
use rusqlite::{OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet};

/// One repository as the `repos` table has it.
pub struct RepoRow {
    pub id: i64,
    pub provider: String,
    pub name: String,
    pub upstream: Option<String>,
    pub checked_at: Option<i64>,
    pub archived_at: Option<i64>,
}

/// What the database knows about one repository, beyond its row.
pub struct RepoFacts {
    /// The commit `main` pointed at when the provider was last asked.
    pub commit: Option<String>,
    pub revisions: u64,
    /// The provider's answer for the newest revision, and when it was fetched.
    pub newest: Option<(String, i64)>,
    pub stored_files: u64,
    pub stored_bytes: u64,
    pub apps: Vec<AppPresence>,
    pub verified: u64,
    pub mismatch: u64,
    pub not_upstream: u64,
    pub unchecked: u64,
    pub ollama_tags: Vec<String>,
    /// Each app's files of it, by the path inside the app's folder.
    pub placed: Vec<(String, String)>,
}

/// One kept revision of a repository.
pub struct RevisionRow {
    pub id: i64,
    pub commit: String,
    pub fetched_at: i64,
    /// The provider's answer for the revision, as it sent it.
    pub info_json: String,
}

/// What an app's folder holds altogether.
pub struct AppTotals {
    pub files: u64,
    pub bytes: u64,
    pub shared_bytes: u64,
    pub models: u64,
}

impl Db {
    /// Every repository, or the one named `only`, by name.
    pub fn repo_rows(&self, only: Option<&str>) -> Result<Vec<RepoRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, provider, name, upstream, checked_at, archived_at FROM repos
             WHERE ?1 IS NULL OR name = ?1 ORDER BY name COLLATE NOCASE, provider",
        )?;
        Ok(stmt
            .query_map([only], |r| {
                Ok(RepoRow {
                    id: r.get(0)?,
                    provider: r.get(1)?,
                    name: r.get(2)?,
                    upstream: r.get(3)?,
                    checked_at: r.get(4)?,
                    archived_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The files a repository refers to that are known to the store, and
    /// their size: by the same rule as archive and `gc` use (`used_by`: a
    /// kept revision, a kept Ollama manifest, or a file it held in an app),
    /// so the numbers a person sees are the files an operation touches.
    pub fn stored_of(&self, repo_id: i64) -> Result<(u64, u64)> {
        let (files, bytes): (i64, i64) = self.conn.query_row(
            &format!(
                "SELECT count(*), COALESCE(sum(b.size), 0) FROM blobs b WHERE {}",
                used_by("= ?1")
            ),
            [repo_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((count(files), count(bytes)))
    }

    pub fn repo_facts(&self, repo_id: i64) -> Result<RepoFacts> {
        let conn = &self.conn;
        let commit: Option<String> = conn
            .query_row(
                "SELECT commit_sha FROM refs WHERE repo_id = ?1 AND name = 'main'",
                [repo_id],
                |r| r.get(0),
            )
            .optional()?;
        let revisions: i64 = conn.query_row(
            "SELECT count(*) FROM revisions WHERE repo_id = ?1",
            [repo_id],
            |r| r.get(0),
        )?;
        let newest: Option<(String, i64)> = conn
            .query_row(
                "SELECT info_json, fetched_at FROM revisions WHERE repo_id = ?1
                 ORDER BY (commit_sha = ?2) DESC, fetched_at DESC, id DESC LIMIT 1",
                params![repo_id, commit],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (stored_files, stored_bytes) = self.stored_of(repo_id)?;
        let mut stmt = conn.prepare(
            "SELECT app, count(*), COALESCE(sum(size), 0),
                    COALESCE(sum(CASE WHEN shared THEN size ELSE 0 END), 0)
             FROM placements WHERE repo_id = ?1 GROUP BY app ORDER BY app",
        )?;
        let apps = stmt
            .query_map([repo_id], |r| {
                Ok(AppPresence {
                    app: app_column(r, 0)?,
                    files: count(r.get(1)?),
                    bytes: count(r.get(2)?),
                    shared_bytes: count(r.get(3)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let counts: (i64, i64, i64, i64) = conn.query_row(
            &format!(
                "SELECT {} FROM placements WHERE repo_id = ?1",
                verify_counts("verify_state")
            ),
            [repo_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let mut stmt = conn
            .prepare("SELECT DISTINCT tag FROM ollama_manifests WHERE repo_id = ?1 ORDER BY tag")?;
        let ollama_tags = stmt
            .query_map([repo_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let mut stmt =
            conn.prepare("SELECT DISTINCT app, rel_path FROM placements WHERE repo_id = ?1")?;
        let placed = stmt
            .query_map([repo_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(RepoFacts {
            commit,
            revisions: count(revisions),
            newest,
            stored_files,
            stored_bytes,
            apps,
            verified: count(counts.0),
            mismatch: count(counts.1),
            not_upstream: count(counts.2),
            unchecked: count(counts.3),
            ollama_tags,
            placed,
        })
    }

    /// For each file of `repo` an app has, by sha256, the apps that have it.
    pub fn holders(&self, repo: &str) -> Result<BTreeMap<String, BTreeSet<App>>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT p.sha256, p.app FROM placements p JOIN repos r ON r.id = p.repo_id
             WHERE r.name = ?1",
        )?;
        let mut holders: BTreeMap<String, BTreeSet<App>> = BTreeMap::new();
        for row in stmt.query_map([repo], |r| Ok((r.get::<_, String>(0)?, app_column(r, 1)?)))? {
            let (sha, app) = row?;
            holders.entry(sha).or_default().insert(app);
        }
        Ok(holders)
    }

    /// The Hugging Face repositories the database knows, each with the apps
    /// that have files of it.
    pub fn presence(&self) -> Result<BTreeMap<String, Vec<App>>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.name, p.app FROM repos r LEFT JOIN placements p ON p.repo_id = r.id
             WHERE r.provider = ?1 GROUP BY r.id, p.app ORDER BY r.name, p.app",
        )?;
        let mut out: BTreeMap<String, Vec<App>> = BTreeMap::new();
        for row in stmt.query_map([PROVIDER_HF], |r| {
            let app = match r.get::<_, Option<String>>(1)? {
                Some(_) => Some(app_column(r, 1)?),
                None => None,
            };
            Ok((r.get::<_, String>(0)?, app))
        })? {
            let (repo, app) = row?;
            out.entry(repo).or_default().extend(app);
        }
        Ok(out)
    }

    /// Every app file that holds a repository's content.
    pub fn placements_of(&self, repo_id: i64) -> Result<Vec<Placement>> {
        let mut stmt = self.conn.prepare(
            "SELECT app, path, rel_path, size, sha256, shared, verify_state, origin
             FROM placements WHERE repo_id = ?1 ORDER BY app, rel_path, path",
        )?;
        Ok(stmt
            .query_map([repo_id], |r| {
                let origin: String = r.get(7)?;
                Ok(Placement {
                    app: app_column(r, 0)?,
                    path: r.get(1)?,
                    rel_path: r.get(2)?,
                    size: count(r.get(3)?),
                    sha256: r.get(4)?,
                    shared: r.get(5)?,
                    verify_state: VerifyState::parse(&r.get::<_, String>(6)?),
                    // Anything else was written by dedupd itself.
                    origin: Origin::parse(&origin).unwrap_or(Origin::Placed),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// A repository's kept revisions, the one `first` names first, then the
    /// newest.
    pub fn revisions_of(&self, repo_id: i64, first: Option<&str>) -> Result<Vec<RevisionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, commit_sha, fetched_at, info_json FROM revisions WHERE repo_id = ?1
             ORDER BY (commit_sha = ?2) DESC, fetched_at DESC, id DESC",
        )?;
        Ok(stmt
            .query_map(params![repo_id, first], |r| {
                Ok(RevisionRow {
                    id: r.get(0)?,
                    commit: r.get(1)?,
                    fetched_at: r.get(2)?,
                    info_json: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The files of one kept revision, by path.
    pub fn revision_entries(&self, revision_id: i64) -> Result<Vec<TreeEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, size, git_oid, lfs_sha256 FROM revision_files
             WHERE revision_id = ?1 ORDER BY path",
        )?;
        Ok(stmt
            .query_map([revision_id], |r| {
                Ok(TreeEntry {
                    path: r.get(0)?,
                    size: count(r.get(1)?),
                    git_oid: r.get(2)?,
                    lfs_sha256: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// What the kept revisions call each file they list: the file's sha256,
    /// or its git id for a small file, to `org/repo: path`. For telling a
    /// person what a stored file is.
    pub fn listed_names(&self) -> Result<BTreeMap<String, String>> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(f.lfs_sha256, f.git_oid), r.name, f.path
             FROM revision_files f
             JOIN revisions v ON v.id = f.revision_id
             JOIN repos r ON r.id = v.repo_id
             ORDER BY v.fetched_at",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut names = BTreeMap::new();
        for row in rows {
            let (id, repo, path) = row?;
            // The newest revision's name for it wins.
            names.insert(id, format!("{repo}: {path}"));
        }
        Ok(names)
    }

    /// Every stored file each repository refers to, as pairs of the
    /// repository's id and the file's sha256, by the rule `stored_of` counts
    /// by. For asking a disk about them after the lock is given back.
    pub fn files_by_repo(&self) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT r.id, b.sha256 FROM repos r JOIN blobs b ON {}",
            used_by("= r.id")
        ))?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// How many repositories the database knows.
    pub fn repo_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM repos", [], |r| r.get(0))?;
        Ok(count(n))
    }

    /// What one app's folder holds, as far as the database knows.
    pub fn app_totals(&self, app: App) -> Result<AppTotals> {
        let row: (i64, i64, i64, i64) = self.conn.query_row(
            "SELECT count(*), COALESCE(sum(size), 0),
                    COALESCE(sum(CASE WHEN shared THEN size ELSE 0 END), 0),
                    count(DISTINCT repo_id)
             FROM placements WHERE app = ?1",
            [app.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        Ok(AppTotals {
            files: count(row.0),
            bytes: count(row.1),
            shared_bytes: count(row.2),
            models: count(row.3),
        })
    }

    /// Bytes of app files that share their disk blocks with the store.
    pub fn shared_bytes(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COALESCE(sum(size), 0) FROM placements WHERE shared = 1",
            [],
            |r| r.get(0),
        )?;
        Ok(count(n))
    }

    /// What each app holds altogether, by app.
    pub fn app_usage(&self) -> Result<Vec<AppUsage>> {
        let mut stmt = self.conn.prepare(
            "SELECT app, count(*), COALESCE(sum(size), 0) FROM placements GROUP BY app ORDER BY app",
        )?;
        Ok(stmt
            .query_map([], |r| {
                Ok(AppUsage {
                    app: app_column(r, 0)?,
                    files: count(r.get(1)?),
                    bytes: count(r.get(2)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every repository in the totals, by provider and name.
    pub fn repositories(&self) -> Result<Vec<Repository>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT r.provider, r.name,
                    (SELECT commit_sha FROM refs WHERE repo_id = r.id AND name = 'main'),
                    count(p.id), COALESCE(sum(p.size), 0),
                    {},
                    r.id, r.archived_at IS NOT NULL
             FROM repos r LEFT JOIN placements p ON p.repo_id = r.id
             GROUP BY r.id ORDER BY r.provider, r.name",
            verify_counts("p.verify_state")
        ))?;
        let rows = stmt
            .query_map([], |r| {
                let n = |i: usize| r.get::<_, i64>(i).map(count);
                Ok((
                    r.get::<_, i64>(9)?,
                    Repository {
                        provider: r.get(0)?,
                        name: r.get(1)?,
                        commit: r.get(2)?,
                        stored_files: 0,
                        stored_bytes: 0,
                        files: n(3)?,
                        bytes: n(4)?,
                        verified: n(5)?,
                        mismatch: n(6)?,
                        not_upstream: n(7)?,
                        unchecked: n(8)?,
                        archived: r.get(10)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, mut repo)| {
                (repo.stored_files, repo.stored_bytes) = self.stored_of(id)?;
                Ok(repo)
            })
            .collect()
    }
}
