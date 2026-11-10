//! Adoption: bring files that an app already holds into the store.
//!
//! Each file is hashed, checked against what the provider says the repository
//! contains, and cloned into the store. The app's own files are only read.

use crate::db::{Db, NewPlacement};
use crate::hash::{FileHashes, hash_file};
use crate::hub::{HubClient, HubError, Snapshot, TreeEntry};
use crate::sources::{LocalFile, LocalRepo};
use crate::store::{Added, Store};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

pub const PROVIDER_HF: &str = "hf";

/// Without a file listing from the provider, files up to this size also get a
/// git blob id, so that small files can be verified later without rehashing.
const GIT_OID_WITHOUT_TREE_MAX: u64 = 64 * 1024 * 1024;

/// How a local file compares with the provider's listing for the repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verify {
    /// Same bytes as the provider lists at the recorded commit.
    Verified,
    /// The provider lists this path with different content. The local file
    /// may come from an older revision, or may be damaged.
    Mismatch,
    /// The provider's listing has no file at this path.
    NotUpstream,
    /// The provider could not be asked.
    Unchecked,
}

impl Verify {
    pub fn as_str(self) -> &'static str {
        match self {
            Verify::Verified => "verified",
            Verify::Mismatch => "mismatch",
            Verify::NotUpstream => "not_upstream",
            Verify::Unchecked => "unchecked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upstream {
    /// The provider answered; the repository's `main` is at this commit.
    Commit(String),
    /// The provider says the repository is missing or not visible.
    NotAvailable(String),
    /// The provider could not be reached or gave an unusable answer.
    Failed(String),
    /// The provider was not asked (`offline`).
    NotAsked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Hashed and cloned into the store.
    Cloned,
    /// Hashed; the store already held the same content.
    AlreadyInStore,
    /// Known from an earlier run and unchanged on disk; not hashed again.
    Unchanged,
    /// Dry run: hashed, nothing written.
    DryRun,
}

#[derive(Debug, Clone)]
pub struct FileOutcome {
    pub repo: String,
    pub rel_path: String,
    pub size: u64,
    pub sha256: String,
    pub verify: Verify,
    pub action: Action,
}

#[derive(Debug, Clone)]
pub struct RepoOutcome {
    pub name: String,
    pub upstream: Upstream,
    pub files: Vec<FileOutcome>,
    /// Files that could not be adopted, as `(path, reason)`.
    pub errors: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// How many files to hash at once.
    pub jobs: usize,
    /// Hash and verify, but write nothing to the store or the database.
    pub dry_run: bool,
    /// Hash every file again, including ones already recorded.
    pub rehash: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            jobs: 4,
            dry_run: false,
            rehash: false,
        }
    }
}

pub enum Event<'a> {
    Repo {
        name: &'a str,
        upstream: &'a Upstream,
        files: usize,
    },
    File(&'a FileOutcome),
    Error {
        path: &'a str,
        reason: &'a str,
    },
}

fn verify(
    entry: Option<&TreeEntry>,
    have_tree: bool,
    sha256: &str,
    git_oid: Option<&str>,
) -> Verify {
    if !have_tree {
        return Verify::Unchecked;
    }
    match entry {
        None => Verify::NotUpstream,
        Some(e) => {
            let same = match &e.lfs_sha256 {
                Some(lfs) => lfs == sha256,
                None => git_oid == Some(e.git_oid.as_str()),
            };
            if same {
                Verify::Verified
            } else {
                Verify::Mismatch
            }
        }
    }
}

/// A listed LFS file with this content under another path. Apps that store
/// files by digest (Ollama) do not keep the repository path.
fn by_content<'a>(
    tree: Option<&'a HashMap<String, TreeEntry>>,
    sha256: &str,
) -> Option<&'a TreeEntry> {
    tree?
        .values()
        .find(|e| e.lfs_sha256.as_deref() == Some(sha256))
}

struct Job {
    repo_index: usize,
    file: LocalFile,
    want_git_oid: bool,
}

struct RepoState {
    repo_id: Option<i64>,
    app: &'static str,
    tree: Option<HashMap<String, TreeEntry>>,
    commit: Option<String>,
}

/// Adopt every file in `repos`. `hub` is `None` to work without asking the
/// provider; files are then recorded as unchecked.
pub async fn adopt(
    db: &std::sync::Mutex<Db>,
    store: &Store,
    hub: Option<&HubClient>,
    repos: Vec<LocalRepo>,
    opts: &Options,
    on_event: &mut dyn FnMut(Event<'_>),
) -> Result<Vec<RepoOutcome>> {
    let mut outcomes: Vec<RepoOutcome> = Vec::new();
    let mut states: Vec<RepoState> = Vec::new();
    let mut jobs: Vec<Job> = Vec::new();

    for (repo_index, repo) in repos.into_iter().enumerate() {
        let revision = repo.revision.as_deref().unwrap_or("main");
        let (upstream, snapshot) = match hub {
            None => (Upstream::NotAsked, None),
            Some(hub) => match hub.snapshot(&repo.name, revision).await {
                Ok(s) => (Upstream::Commit(s.commit.clone()), Some(s)),
                Err(e @ HubError::NotAvailable { .. }) => {
                    (Upstream::NotAvailable(e.to_string()), None)
                }
                Err(e) => (Upstream::Failed(e.to_string()), None),
            },
        };
        on_event(Event::Repo {
            name: &repo.name,
            upstream: &upstream,
            files: repo.files.len(),
        });

        let repo_id = if opts.dry_run {
            None
        } else {
            let id = db
                .lock()
                .expect("database lock")
                .upsert_repo(PROVIDER_HF, &repo.name)?;
            if let Some(snap) = &snapshot {
                // A branch name is recorded as a ref; a commit id is not one.
                db.lock().expect("database lock").record_snapshot(
                    id,
                    (!crate::db::is_commit(revision)).then_some(revision),
                    snap,
                )?;
            }
            Some(id)
        };
        let commit = snapshot.as_ref().map(|s| s.commit.clone());
        let tree = snapshot.map(|Snapshot { entries, .. }| {
            entries
                .into_iter()
                .map(|e| (e.path.clone(), e))
                .collect::<HashMap<_, _>>()
        });

        let mut outcome = RepoOutcome {
            name: repo.name.clone(),
            upstream,
            files: Vec::new(),
            errors: Vec::new(),
        };

        for file in repo.files {
            let entry = tree.as_ref().and_then(|t| t.get(&file.rel_path));
            let want_git_oid = match (&tree, entry) {
                (Some(_), Some(e)) => e.lfs_sha256.is_none(),
                (Some(_), None) => false,
                (None, _) => file.fingerprint.size <= GIT_OID_WITHOUT_TREE_MAX,
            };

            // A file recorded earlier and untouched since needs no new hash,
            // unless checking it against the listing takes a git blob id that
            // was not computed the first time.
            let needs_git_oid_now = entry.is_some_and(|e| e.lfs_sha256.is_none());
            let abs = file.abs_path.to_string_lossy().into_owned();
            let known = if opts.rehash {
                None
            } else {
                db.lock().expect("database lock").placement(&abs)?
            };
            if let (Some(known), Some(repo_id)) = (known, repo_id) {
                let unchanged = known.size == file.fingerprint.size
                    && known.mtime_ns == file.fingerprint.mtime_ns
                    && store.contains(&known.sha256)
                    && (!needs_git_oid_now || known.git_oid.is_some());
                if unchanged {
                    let v = verify(
                        entry.or_else(|| by_content(tree.as_ref(), &known.sha256)),
                        tree.is_some(),
                        &known.sha256,
                        known.git_oid.as_deref(),
                    );
                    // An answer from the provider replaces an older one; no
                    // answer leaves the earlier result in place.
                    if v != Verify::Unchecked && v.as_str() != known.verify_state {
                        db.lock()
                            .expect("database lock")
                            .upsert_placement(&NewPlacement {
                                app: repo.app,
                                path: &abs,
                                repo_id,
                                rel_path: &file.rel_path,
                                sha256: &known.sha256,
                                size: known.size,
                                mtime_ns: known.mtime_ns,
                                origin: "adopted",
                                verify_state: v.as_str(),
                                verified_commit: (v == Verify::Verified)
                                    .then_some(commit.as_deref())
                                    .flatten(),
                                shared: known.shared,
                            })?;
                    }
                    let recorded = if v == Verify::Unchecked {
                        match known.verify_state.as_str() {
                            "verified" => Verify::Verified,
                            "mismatch" => Verify::Mismatch,
                            "not_upstream" => Verify::NotUpstream,
                            _ => Verify::Unchecked,
                        }
                    } else {
                        v
                    };
                    let done = FileOutcome {
                        repo: repo.name.clone(),
                        rel_path: file.rel_path,
                        size: known.size,
                        sha256: known.sha256,
                        verify: recorded,
                        action: Action::Unchanged,
                    };
                    on_event(Event::File(&done));
                    outcome.files.push(done);
                    continue;
                }
            }
            jobs.push(Job {
                repo_index,
                file,
                want_git_oid,
            });
        }

        outcomes.push(outcome);
        states.push(RepoState {
            repo_id,
            app: repo.app,
            tree,
            commit,
        });
    }

    // Hash in the background, a few files at a time; record results here as
    // they finish so the database is only ever touched from this task.
    let limit = Arc::new(Semaphore::new(opts.jobs.max(1)));
    let mut running: JoinSet<(Job, std::io::Result<FileHashes>)> = JoinSet::new();
    for job in jobs {
        let limit = limit.clone();
        running.spawn(async move {
            let _permit = limit
                .acquire_owned()
                .await
                .expect("semaphore is never closed");
            let path = job.file.abs_path.clone();
            let want = job.want_git_oid;
            let hashed = tokio::task::spawn_blocking(move || hash_file(&path, want))
                .await
                .unwrap_or_else(|e| Err(std::io::Error::other(e)));
            (job, hashed)
        });
    }

    while let Some(joined) = running.join_next().await {
        let (job, hashed) = joined?;
        let state = &states[job.repo_index];
        let outcome = &mut outcomes[job.repo_index];
        let abs = job.file.abs_path.to_string_lossy().into_owned();

        let result = (|| -> Result<FileOutcome> {
            let hashes = hashed?;
            let entry = state.tree.as_ref().and_then(|t| t.get(&job.file.rel_path));
            let v = verify(
                entry.or_else(|| by_content(state.tree.as_ref(), &hashes.sha256)),
                state.tree.is_some(),
                &hashes.sha256,
                hashes.git_oid.as_deref(),
            );

            let action = match state.repo_id {
                None => Action::DryRun,
                Some(repo_id) => {
                    let added = store.add_clone(
                        &job.file.abs_path,
                        &hashes.sha256,
                        job.file.fingerprint,
                    )?;
                    db.lock().expect("database lock").upsert_blob(
                        &hashes.sha256,
                        hashes.size,
                        hashes.git_oid.as_deref(),
                    )?;
                    db.lock()
                        .expect("database lock")
                        .upsert_placement(&NewPlacement {
                            app: state.app,
                            path: &abs,
                            repo_id,
                            rel_path: &job.file.rel_path,
                            sha256: &hashes.sha256,
                            size: hashes.size,
                            mtime_ns: job.file.fingerprint.mtime_ns,
                            origin: "adopted",
                            verify_state: v.as_str(),
                            verified_commit: (v == Verify::Verified)
                                .then_some(state.commit.as_deref())
                                .flatten(),
                            shared: added == Added::Cloned,
                        })?;
                    match added {
                        Added::Cloned => Action::Cloned,
                        Added::AlreadyPresent => Action::AlreadyInStore,
                    }
                }
            };
            Ok(FileOutcome {
                repo: outcome.name.clone(),
                rel_path: job.file.rel_path.clone(),
                size: hashes.size,
                sha256: hashes.sha256,
                verify: v,
                action,
            })
        })();

        match result {
            Ok(done) => {
                on_event(Event::File(&done));
                outcome.files.push(done);
            }
            Err(e) => {
                let reason = format!("{e:#}");
                on_event(Event::Error {
                    path: &abs,
                    reason: &reason,
                });
                outcome.errors.push((abs, reason));
            }
        }
    }

    for outcome in &mut outcomes {
        outcome.files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    }
    Ok(outcomes)
}

/// List what the given apps hold, and archive the Ollama manifests found
/// there: Hugging Face generates those and they exist nowhere else.
pub fn scan_apps(
    db: &std::sync::Mutex<Db>,
    dirs: &crate::place::AppDirs,
    apps: &[crate::place::App],
) -> Result<Vec<LocalRepo>> {
    use crate::place::App;
    use crate::sources::{bionic, hf_cache, ollama};
    let mut repos = Vec::new();
    for app in apps {
        match app {
            App::Bionic if dirs.bionic.is_dir() => repos.extend(bionic::scan(&dirs.bionic)?),
            App::Hf => repos.extend(hf_cache::scan(&dirs.hf_cache)?),
            App::Ollama => {
                let db = db.lock().expect("database lock");
                for m in ollama::manifests(&dirs.ollama) {
                    let repo_id = db.upsert_repo(PROVIDER_HF, &m.repo)?;
                    if db.manifest(repo_id, &m.tag, None)?.map(|(_, b)| b) != Some(m.body.clone()) {
                        db.save_manifest(
                            repo_id,
                            &m.tag,
                            "adopted",
                            crate::ollama::MANIFEST_TYPE,
                            &m.body,
                        )?;
                    }
                }
                drop(db);
                repos.extend(ollama::scan(&dirs.ollama)?);
            }
            App::Bionic => {}
        }
    }
    Ok(repos)
}
