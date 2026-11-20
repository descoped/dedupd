//! `pull` and `place`: get a repository revision into the store, then put it
//! where the chosen apps look for it.

use crate::db::PROVIDER_HF;
use crate::engine::{Engine, blocking};
use crate::fetch::{Progress, Unavailable, Wait, Watch};
use crate::hub::{Snapshot, TreeEntry};
use crate::names::parse_model;
use crate::ollama_mlx;
use crate::place::{self, Dest, Item};
use crate::profile;
use crate::select::{self, Selection};
use anyhow::{Context, Result, bail};
use dedupd_api::app::App;
use dedupd_api::jobs::{NotActivated, PlacedApp, PullResult};
use std::sync::atomic::Ordering;

mod into_ollama;
use into_ollama::place_into_ollama;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Downloads to run at once when the caller does not say.
pub const DEFAULT_JOBS: usize = 4;

#[derive(Debug, Clone)]
pub struct PullRequest {
    /// `org/name`.
    pub repo: String,
    /// A branch, tag or commit.
    pub revision: String,
    pub selection: Selection,
    pub apps: Vec<App>,
    /// Use only what is archived; do not contact the provider.
    pub offline: bool,
    /// Downloads to run at once.
    pub jobs: usize,
}

impl PullRequest {
    /// A request for `model`, written `org/repo[:quant][@revision]` as on
    /// the command line. `include` holds extra file patterns.
    pub fn from_spec(
        model: &str,
        include: Vec<String>,
        apps: Vec<App>,
        offline: bool,
        jobs: usize,
    ) -> Result<Self> {
        let (repo, quant, revision) = parse_model(model)?;
        Ok(Self {
            repo,
            revision,
            selection: Selection { quant, include },
            apps,
            offline,
            jobs,
        })
    }
}

/// What placing a pull's files did, app by app.
#[derive(Debug, Default)]
pub struct Placement {
    pub placed: Vec<PlacedApp>,
    pub skipped: Vec<NotActivated>,
    pub notes: Vec<String>,
}

/// Follows a pull. Files are named by their place in the plan.
pub trait PullWatch: Send + Sync {
    /// The bytes of file `index` that are on disk changed by `delta`.
    fn bytes(&self, index: usize, delta: i64);
    /// This many bytes just came over the network.
    fn received(&self, _bytes: u64) {}
    fn waiting(&self, _index: usize, _why: Wait) {}
    fn running(&self, _index: usize) {}
    fn verifying(&self, _index: usize) {}
    /// File `index` is in the store.
    fn stored(&self, _index: usize) {}
    /// This many files are not in the store and are about to be downloaded.
    fn fetching(&self, _files: usize) {}
}

impl PullWatch for Progress {
    fn bytes(&self, _index: usize, delta: i64) {
        Watch::bytes(self, delta);
    }
}

/// One file of a pull, as the fetcher sees its watcher.
struct FileWatch {
    watch: Arc<dyn PullWatch>,
    index: usize,
}

impl Watch for FileWatch {
    fn bytes(&self, delta: i64) {
        self.watch.bytes(self.index, delta);
    }
    fn received(&self, bytes: u64) {
        self.watch.received(bytes);
    }
    fn waiting(&self, why: Wait) {
        self.watch.waiting(self.index, why);
    }
    fn running(&self) {
        self.watch.running(self.index);
    }
    fn verifying(&self) {
        self.watch.verifying(self.index);
    }
}

/// What a pull will do: the revision it works on and the files it needs.
pub struct Plan {
    pub repo_id: i64,
    pub snap: Snapshot,
    /// The provider could not answer and the archived copy is used.
    pub from_archive: bool,
    /// Why the archived copy is used, for the owner.
    pub note: Option<String>,
    /// The files of the revision the request selects.
    pub entries: Vec<TreeEntry>,
    pub bytes: u64,
}

/// The revision a pull works on.
struct Resolved {
    repo_id: i64,
    snap: Snapshot,
    from_archive: bool,
    note: Option<String>,
}

/// Find the revision: ask the provider, and fall back to the archive when it
/// cannot answer or no longer has the repository. `revision` is the request's
/// own, or the commit an earlier run of the same pull settled on.
async fn resolve(engine: &Engine, req: &PullRequest, revision: &str) -> Result<Resolved> {
    let mut upstream_error = None;
    let fresh = if req.offline {
        None
    } else {
        match engine.hub.snapshot(&req.repo, revision).await {
            Ok(s) => Some(s),
            Err(e) => {
                upstream_error = Some(e);
                None
            }
        }
    };
    let mut db = engine.db();
    if let Some(snap) = fresh {
        let repo_id = db.archive_snapshot(PROVIDER_HF, &req.repo, revision, &snap)?;
        return Ok(Resolved {
            repo_id,
            snap,
            from_archive: false,
            note: None,
        });
    }
    match (
        db.archived(PROVIDER_HF, &req.repo, revision)?,
        upstream_error,
    ) {
        (Some((repo_id, snap)), e) => Ok(Resolved {
            repo_id,
            snap,
            from_archive: true,
            note: e.map(|e| format!("{e}; using the kept copy")),
        }),
        // The provider's own error stays in the chain, so a caller can tell
        // a repository that is gone from a provider that cannot be reached.
        (None, Some(e)) => {
            Err(anyhow::Error::new(e).context(format!("{} is not kept here", req.repo)))
        }
        (None, None) => bail!(
            "{}@{revision} is not kept here, and --offline was given",
            req.repo
        ),
    }
}

/// Fetch whatever of the plan's files the store does not hold yet. Returns
/// one item per file and the number downloaded.
pub async fn fetch(
    engine: &Arc<Engine>,
    req: &PullRequest,
    plan: &Plan,
    watch: Arc<dyn PullWatch>,
) -> Result<(Vec<Item>, usize)> {
    let commit = &plan.snap.commit;
    let mut items = Vec::new();
    let mut missing = Vec::new();
    for (index, e) in plan.entries.iter().cloned().enumerate() {
        let known = engine.db().blob_for_entry(&e)?;
        let Some(b) = known else {
            missing.push((index, e));
            continue;
        };
        // A copy that cannot be brought back from the archive disk (detached
        // mid-copy, damaged) is downloaded again when that is allowed.
        let sha = b.sha256.clone();
        let local = match blocking(engine, move |engine| engine.store.ensure_local(&sha)).await {
            Err(e) if req.offline => return Err(e),
            other => other.unwrap_or(false),
        };
        if local {
            watch.bytes(index, b.size as i64);
            watch.stored(index);
            items.push(Item {
                rel_path: e.path.clone(),
                sha256: b.sha256,
                size: b.size,
                etag: e.etag().to_string(),
            });
        } else {
            missing.push((index, e));
        }
    }
    if missing.is_empty() {
        return Ok((items, 0));
    }
    if req.offline {
        bail!(
            "{} files are not in the store, and --offline was given (first: {})",
            missing.len(),
            missing[0].1.path
        );
    }

    let downloaded = missing.len();
    watch.fetching(downloaded);
    let limit = Arc::new(Semaphore::new(req.jobs.max(1)));
    let mut running: JoinSet<Result<Item>> = JoinSet::new();
    for (index, e) in missing {
        let (engine, limit) = (engine.clone(), limit.clone());
        let url = engine.fetcher.resolve_url(&req.repo, commit, &e.path);
        let file = FileWatch {
            watch: watch.clone(),
            index,
        };
        running.spawn(async move {
            let _permit = limit.acquire_owned().await.expect("semaphore is open");
            let got = engine
                .fetcher
                .fetch(&engine.store, &url, Some(e.size), &e.expect(), &file)
                .await
                .with_context(|| format!("fetching {}", e.path))?;
            engine
                .db()
                .upsert_blob(&got.sha256, got.size, got.git_oid.as_deref())?;
            file.watch.stored(index);
            Ok(Item {
                etag: e.etag().to_string(),
                rel_path: e.path,
                sha256: got.sha256,
                size: got.size,
            })
        });
    }
    // The first failure is the one reported, and the other files are
    // still fetched to their end, on purpose: what is downloaded is kept,
    // so a second try after the owner has dealt with the one file has only
    // that file left. (A caller that drops this future ends them all.)
    let mut first_error = None;
    while let Some(joined) = running.join_next().await {
        match joined? {
            Ok(item) => items.push(item),
            Err(e) => first_error = first_error.or(Some(e)),
        }
    }
    if let Some(e) = first_error {
        return Err(e);
    }
    items.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok((items, downloaded))
}

fn dest<'a>(engine: &'a Engine, repo_id: i64, repo: &'a str) -> Dest<'a> {
    Dest {
        db: &engine.db,
        store: &engine.store,
        dirs: &engine.dirs,
        repo_id,
        repo,
    }
}

/// Settle what a pull will do. `pinned` is the commit an earlier run of the
/// same pull settled on; a pull that is continued keeps to it, so its files
/// never come from two versions of the repository.
pub async fn plan(engine: &Engine, req: &PullRequest, pinned: Option<&str>) -> Result<Plan> {
    let Resolved {
        repo_id,
        snap,
        from_archive,
        note,
    } = resolve(engine, req, pinned.unwrap_or(&req.revision)).await?;
    let entries = select::select(&snap.entries, &req.selection)?;
    let bytes = entries.iter().map(|e| e.size).sum();
    Ok(Plan {
        repo_id,
        snap,
        from_archive,
        note,
        entries,
        bytes,
    })
}

/// Put the plan's files, now in the store, into `req.apps`. Returns what was
/// placed and the things that did not stop it but the owner should know.
pub async fn place(
    engine: &Arc<Engine>,
    req: &PullRequest,
    plan: &Plan,
    items: Vec<Item>,
) -> Result<Placement> {
    let (repo_id, snap) = (plan.repo_id, &plan.snap);
    // The files are in the store again, so the model is no longer archived.
    engine.db().set_archived(repo_id, None)?;
    let mut done = Placement::default();
    // Placement hashes and copies files, so it runs off the async runtime.
    let items = Arc::new(items);
    let fresh = !plan.from_archive;
    for app in &req.apps {
        let (repo, commit, items) = (req.repo.clone(), snap.commit.clone(), items.clone());
        let result = match app {
            App::Bionic => blocking(engine, move |engine| {
                place::place_bionic(&dest(engine, repo_id, &repo), &commit, &items)
            })
            .await
            .map(Ok),
            App::Hf => {
                let ref_name = crate::hub::ref_name(&req.revision).map(str::to_string);
                blocking(engine, move |engine| {
                    place::place_hf(
                        &dest(engine, repo_id, &repo),
                        &commit,
                        ref_name.as_deref(),
                        fresh,
                        &items,
                    )
                })
                .await
                .map(Ok)
            }
            App::Ollama => {
                place_into_ollama(engine, req, repo_id, snap, &plan.entries, &items).await
            }
        };
        let result = match result {
            Ok(Ok(result)) => result,
            Ok(Err(skipped)) => {
                done.skipped.push(skipped);
                continue;
            }
            // One app's failure is that app's: the others are still asked,
            // and the history is told of all of them. Worth another try,
            // unless the app's folder is on a volume nothing can be cloned
            // to.
            Err(e) => {
                done.skipped.push(NotActivated {
                    app: *app,
                    reason: format!("{e:#}"),
                    temporary: e.downcast_ref::<crate::clone::OtherVolume>().is_none(),
                });
                continue;
            }
        };
        if !result.conflicts.is_empty() {
            for c in &result.conflicts {
                done.notes.push(format!("left alone: {c}"));
            }
            done.skipped.push(NotActivated::lasting(
                *app,
                format!(
                    "files in its folder with other content were left alone ({})",
                    result.conflicts.len()
                ),
            ));
        }
        done.placed.push(PlacedApp {
            app: *app,
            cloned: result.cloned as u64,
            present: result.present as u64,
            conflicts: result.conflicts,
        });
    }
    // Every caller's activations are in the owner's history.
    let (repo, apps) = (req.repo.clone(), req.apps.clone());
    let (placed, skipped) = (done.placed.clone(), done.skipped.clone());
    blocking(engine, move |engine| {
        crate::activity::record_activations(engine.store.home(), &repo, &apps, &placed, &skipped);
        Ok(())
    })
    .await?;
    Ok(done)
}

/// Fetch a revision into the store and place it into `req.apps`.
pub async fn pull(
    engine: &Arc<Engine>,
    req: &PullRequest,
    progress: &Arc<Progress>,
) -> Result<PullResult> {
    let plan = plan(engine, req, None).await?;
    progress.total.fetch_add(plan.bytes, Ordering::Relaxed);
    let (items, downloaded) = fetch(engine, req, &plan, progress.clone())
        .await
        .map_err(|e| gone_upstream(e, req, &plan))?;
    let files = items.len();
    let placement = place(engine, req, &plan, items).await?;
    Ok(outcome(req, plan, files, downloaded, placement))
}

/// Say why a file could not be had when the repository itself is gone.
pub fn gone_upstream(e: anyhow::Error, req: &PullRequest, plan: &Plan) -> anyhow::Error {
    if plan.from_archive && e.downcast_ref::<Unavailable>().is_some() {
        return e.context(format!(
            "{} is gone upstream and this file was never archived",
            req.repo
        ));
    }
    e
}

/// What a finished pull did.
pub fn outcome(
    req: &PullRequest,
    plan: Plan,
    files: usize,
    downloaded: usize,
    placement: Placement,
) -> PullResult {
    PullResult {
        repo: req.repo.clone(),
        commit: plan.snap.commit,
        from_archive: plan.from_archive,
        files: files as u64,
        bytes: plan.bytes,
        downloaded: downloaded as u64,
        placed: placement.placed,
        skipped: placement.skipped,
        notes: plan.note.into_iter().chain(placement.notes).collect(),
    }
}
