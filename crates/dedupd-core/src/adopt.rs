//! Adoption: bring files that an app already holds into the store.
//!
//! Each file is hashed, checked against what the provider says the repository
//! contains, and cloned into the store. The app's own files are only read.

use crate::db::{Db, NewPlacement, ORIGIN_ADOPTED, PROVIDER_HF, lock};
use crate::hash::{FileHashes, hash_file};
use crate::hub::{HubClient, HubError, Snapshot, TreeEntry};
use crate::sources::{LocalFile, LocalRepo};
use crate::store::{Added, Fingerprint, Store};
use anyhow::Result;
use dedupd_api::app::App;
use dedupd_api::library::VerifyState;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// The User-Agent key under which a manifest found in Ollama's own folder is
/// archived, since no request fetched it.
const MANIFEST_FROM_APP: &str = "adopted";

/// Without a file listing from the provider, files up to this size also get a
/// git blob id, so that small files can be verified later without rehashing.
const GIT_OID_WITHOUT_TREE_MAX: u64 = 64 * 1024 * 1024;

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
    pub verify: VerifyState,
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
) -> VerifyState {
    if !have_tree {
        return VerifyState::Unchecked;
    }
    match entry {
        None => VerifyState::NotUpstream,
        Some(e) => {
            let same = match &e.lfs_sha256 {
                Some(lfs) => lfs == sha256,
                None => git_oid == Some(e.git_oid.as_str()),
            };
            if same {
                VerifyState::Verified
            } else {
                VerifyState::Mismatch
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

/// How the file at `rel_path` with this content compares with the listing.
fn check(
    tree: Option<&HashMap<String, TreeEntry>>,
    rel_path: &str,
    sha256: &str,
    git_oid: Option<&str>,
) -> VerifyState {
    let entry = tree
        .and_then(|t| t.get(rel_path))
        .or_else(|| by_content(tree, sha256));
    verify(entry, tree.is_some(), sha256, git_oid)
}

/// A file waiting to be hashed.
struct Job {
    repo_index: usize,
    file: LocalFile,
    want_git_oid: bool,
}

struct RepoState {
    /// `None` in a dry run, which records nothing.
    repo_id: Option<i64>,
    app: &'static str,
    tree: Option<HashMap<String, TreeEntry>>,
    commit: Option<String>,
}

/// One adopted file, as it goes into the database.
struct Adopted<'a> {
    path: &'a str,
    rel_path: &'a str,
    sha256: &'a str,
    fingerprint: Fingerprint,
    verify: VerifyState,
    shared: bool,
}

impl RepoState {
    /// The database row for an adopted file of this repository.
    fn placement<'a>(&'a self, repo_id: i64, file: &Adopted<'a>) -> NewPlacement<'a> {
        NewPlacement {
            app: self.app,
            path: file.path,
            repo_id,
            rel_path: file.rel_path,
            sha256: file.sha256,
            size: file.fingerprint.size,
            mtime_ns: file.fingerprint.mtime_ns,
            origin: ORIGIN_ADOPTED,
            verify_state: file.verify.as_str(),
            verified_commit: (file.verify == VerifyState::Verified)
                .then_some(self.commit.as_deref())
                .flatten(),
            shared: file.shared,
            derived: None,
        }
    }
}

/// Ask the provider about one repository, archive its answer, and settle the
/// files that are recorded and unchanged. Returns the files left to hash.
async fn plan_repo(
    db: &Mutex<Db>,
    store: &Store,
    hub: Option<&HubClient>,
    repo_index: usize,
    repo: LocalRepo,
    opts: &Options,
    on_event: &mut (dyn FnMut(Event<'_>) + Send),
) -> Result<(RepoState, RepoOutcome, Vec<Job>)> {
    let revision = repo.revision.as_deref().unwrap_or("main");
    // Only Hugging Face repositories can be checked against the Hub, and
    // only by a name and a revision that are ones: both come from folder
    // names here and go into the address asked. Anything else is adopted
    // without asking.
    let hub = hub.filter(|_| {
        repo.provider == PROVIDER_HF
            && crate::names::is_repo_name(&repo.name)
            && (revision == "main" || crate::hub::is_commit(revision))
    });
    let (upstream, snapshot) = match hub {
        None => (Upstream::NotAsked, None),
        Some(hub) => match hub.snapshot(&repo.name, revision).await {
            Ok(s) => (Upstream::Commit(s.commit.clone()), Some(s)),
            Err(e @ HubError::NotAvailable { .. }) => (Upstream::NotAvailable(e.to_string()), None),
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
        Some(match &snapshot {
            Some(snap) => lock(db).archive_snapshot(repo.provider, &repo.name, revision, snap)?,
            None => lock(db).upsert_repo(repo.provider, &repo.name)?,
        })
    };
    let commit = snapshot.as_ref().map(|s| s.commit.clone());
    let tree = snapshot.map(|Snapshot { entries, .. }| {
        entries
            .into_iter()
            .map(|e| (e.path.clone(), e))
            .collect::<HashMap<_, _>>()
    });
    let state = RepoState {
        repo_id,
        app: repo.app,
        tree,
        commit,
    };
    let tree = state.tree.as_ref();

    let mut outcome = RepoOutcome {
        name: repo.name.clone(),
        upstream,
        files: Vec::new(),
        errors: Vec::new(),
    };
    let mut jobs = Vec::new();

    for file in repo.files {
        let entry = tree.and_then(|t| t.get(&file.rel_path));
        let want_git_oid = match (tree, entry) {
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
            lock(db).placement(&abs)?
        };
        // Also in a dry run, where there is no repository id to write
        // under: a file that is recorded and unchanged is not read again.
        // The store on the disk of the app's folder is the one an adopted
        // file is cloned into; with none there, the primary store refuses
        // the clone as it always has.
        if let Some(known) = known
            && known.fingerprint() == file.fingerprint
            && store_for(store, &file.abs_path).contains(&known.sha256)
            && (!needs_git_oid_now || known.git_oid.is_some())
        {
            let v = check(
                tree,
                &file.rel_path,
                &known.sha256,
                known.git_oid.as_deref(),
            );
            // An answer from the provider replaces an older one; no
            // answer leaves the earlier result in place.
            if let Some(repo_id) = state.repo_id
                && v != VerifyState::Unchecked
                && v.as_str() != known.verify_state
            {
                lock(db).upsert_placement(&state.placement(
                    repo_id,
                    &Adopted {
                        path: &abs,
                        rel_path: &file.rel_path,
                        sha256: &known.sha256,
                        fingerprint: known.fingerprint(),
                        verify: v,
                        shared: known.shared,
                    },
                ))?;
            }
            let recorded = if v == VerifyState::Unchecked {
                VerifyState::parse(&known.verify_state)
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
        jobs.push(Job {
            repo_index,
            file,
            want_git_oid,
        });
    }
    Ok((state, outcome, jobs))
}

/// Verify a hashed file, clone it into the store and record it. In a dry
/// run only the verification happens.
fn record_hashed(
    db: &Mutex<Db>,
    store: &Store,
    state: &RepoState,
    repo: &str,
    file: &LocalFile,
    hashed: std::io::Result<FileHashes>,
) -> Result<FileOutcome> {
    let hashes = hashed?;
    let tree = state.tree.as_ref();
    let v = check(
        tree,
        &file.rel_path,
        &hashes.sha256,
        hashes.git_oid.as_deref(),
    );

    let action = match state.repo_id {
        None => Action::DryRun,
        Some(repo_id) => {
            let added = store_for(store, &file.abs_path).add_clone(
                &file.abs_path,
                &hashes.sha256,
                file.fingerprint,
            )?;
            lock(db).upsert_blob(&hashes.sha256, hashes.size, hashes.git_oid.as_deref())?;
            lock(db).upsert_placement(&state.placement(
                repo_id,
                &Adopted {
                    path: &file.abs_path.to_string_lossy(),
                    rel_path: &file.rel_path,
                    sha256: &hashes.sha256,
                    fingerprint: Fingerprint {
                        size: hashes.size,
                        mtime_ns: file.fingerprint.mtime_ns,
                    },
                    verify: v,
                    shared: added == Added::Stored,
                },
            ))?;
            match added {
                Added::Stored => Action::Cloned,
                Added::AlreadyPresent => Action::AlreadyInStore,
            }
        }
    };
    Ok(FileOutcome {
        repo: repo.to_string(),
        rel_path: file.rel_path.clone(),
        size: hashes.size,
        sha256: hashes.sha256,
        verify: v,
        action,
    })
}

/// Adopt every file in `repos`. `hub` is `None` to work without asking the
/// provider; files are then recorded as unchecked.
pub async fn adopt(
    db: &Mutex<Db>,
    store: &Store,
    hub: Option<&HubClient>,
    repos: Vec<LocalRepo>,
    opts: &Options,
    on_event: &mut (dyn FnMut(Event<'_>) + Send),
) -> Result<Vec<RepoOutcome>> {
    let mut outcomes: Vec<RepoOutcome> = Vec::new();
    let mut states: Vec<RepoState> = Vec::new();
    let mut jobs: Vec<Job> = Vec::new();

    for (repo_index, repo) in repos.into_iter().enumerate() {
        let (state, outcome, to_hash) =
            plan_repo(db, store, hub, repo_index, repo, opts, on_event).await?;
        states.push(state);
        outcomes.push(outcome);
        jobs.extend(to_hash);
    }

    // Hash in the background, a few files at a time; record results here as
    // they finish so the database is only ever touched from this task.
    // Recording one file is short (a clone and a rename on the internal
    // disk, two database writes) and the task yields between files.
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
        match record_hashed(db, store, state, &outcome.name, &job.file, hashed) {
            Ok(done) => {
                on_event(Event::File(&done));
                outcome.files.push(done);
            }
            Err(e) => {
                let abs = job.file.abs_path.to_string_lossy().into_owned();
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

/// What `recognise_stored` found.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Recognised {
    /// Files recorded again.
    pub files: u64,
    pub bytes: u64,
    /// Files whose content does not hash to their name; left alone.
    pub damaged: u64,
    /// Good files no repository the database knows refers to; left out of
    /// the records, as before.
    pub unused: u64,
}

/// Record the files that are in the store but not in the database. After
/// the database is removed (there are no migrations), a scan of the app
/// folders finds only what the apps hold; the store also has the files no
/// app downloaded, such as a model's README, and a model without them
/// would count as not fully stored.
///
/// Each such file is read in full. It is recorded only when its content
/// hashes to its name and a repository the database knows refers to it,
/// so a recorded file is never one `gc` would offer to remove. A file that
/// is damaged or that nothing refers to is put in `looked_at` with its size
/// and time, and is not read again until one of them changes.
pub fn recognise_stored(
    db: &Mutex<Db>,
    store: &Store,
    looked_at: &mut HashMap<String, Fingerprint>,
) -> Result<Recognised> {
    let known: HashSet<String> = lock(db)
        .all_blobs()?
        .into_iter()
        .map(|b| b.sha256)
        .collect();
    looked_at.retain(|sha, _| !known.contains(sha));
    let mut found = Recognised::default();
    let stored: Vec<PathBuf> = store
        .attached_stores()
        .into_iter()
        .flat_map(|root| {
            let view = store.named(&root.name).ok();
            view.map(|v| {
                v.stored_hashes()
                    .into_iter()
                    .map(|sha| v.blob_path(&sha))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        })
        .collect();
    for path in stored {
        let Some(sha) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if known.contains(&sha) {
            continue;
        }
        // Gone a moment later (`gc`), or unreadable: not recorded.
        let Ok(seen) = Fingerprint::of(&path) else {
            continue;
        };
        if looked_at.get(&sha) == Some(&seen) {
            continue;
        }
        let Ok(hashes) = hash_file(&path, seen.size <= GIT_OID_WITHOUT_TREE_MAX) else {
            continue;
        };
        if hashes.sha256 != sha {
            found.damaged += 1;
            looked_at.insert(sha, seen);
            continue;
        }
        if lock(db).record_blob_if_used(&sha, hashes.size, hashes.git_oid.as_deref())? {
            found.files += 1;
            found.bytes += hashes.size;
            looked_at.remove(&sha);
        } else {
            found.unused += 1;
            looked_at.insert(sha, seen);
        }
    }
    Ok(found)
}

/// List what the given apps hold, and archive the Ollama manifests found
/// there: Hugging Face generates those and they exist nowhere else.
pub fn scan_apps(
    db: &Mutex<Db>,
    dirs: &crate::place::AppDirs,
    apps: &[App],
) -> Result<Vec<LocalRepo>> {
    use crate::sources::{bionic, hf_cache, ollama};
    let mut repos = Vec::new();
    for app in apps {
        match app {
            App::Bionic if dirs.bionic.is_dir() => repos.extend(bionic::scan(&dirs.bionic)?),
            App::Hf => {
                // The cache records which commit each branch name was at.
                let refs = hf_cache::refs(&dirs.hf_cache)?;
                let db = lock(db);
                for (repo, name, commit) in refs {
                    let repo_id = db.upsert_repo(PROVIDER_HF, &repo)?;
                    if db.ref_commit(repo_id, &name)?.is_none() {
                        db.set_ref(repo_id, &name, &commit)?;
                    }
                }
                drop(db);
                repos.extend(hf_cache::scan(&dirs.hf_cache)?);
            }
            App::Ollama => {
                let found = ollama::manifests(&dirs.ollama)?;
                let db = lock(db);
                // A file dedupd derived from a stored one is its own work.
                // Neither it nor a manifest that names it is something to
                // archive: the store holds what it was made from.
                let derived = db.derived_files()?;
                let names_derived = |body: &str| {
                    crate::ollama::Manifest::parse(body).is_ok_and(|m| {
                        m.all()
                            .any(|l| l.sha256().is_ok_and(|sha| derived.contains(sha)))
                    })
                };
                for m in found {
                    if names_derived(&m.body) {
                        continue;
                    }
                    let repo_id = db.upsert_repo(m.provider, &m.repo)?;
                    let archived = db.manifest(repo_id, &m.tag, None)?;
                    if archived.map(|(_, body)| body).as_ref() != Some(&m.body) {
                        db.save_manifest(
                            repo_id,
                            &m.tag,
                            MANIFEST_FROM_APP,
                            crate::ollama::MANIFEST_TYPE,
                            &m.body,
                        )?;
                    }
                }
                drop(db);
                let mut scanned = ollama::scan(&dirs.ollama)?;
                for repo in &mut scanned {
                    repo.files.retain(|f| {
                        !f.rel_path
                            .strip_prefix("blobs/sha256-")
                            .is_some_and(|sha| derived.contains(sha))
                    });
                }
                scanned.retain(|r| !r.files.is_empty());
                repos.extend(scanned);
            }
            App::Bionic => {}
        }
    }
    Ok(repos)
}

/// The store an app's file is adopted into: the one on its disk, or the
/// primary store when there is none there (which then refuses the clone).
fn store_for(store: &Store, file: &Path) -> Store {
    store.for_folder(file).unwrap_or_else(|_| store.primary())
}
