//! `pull` and `place`: get a repository revision into the store, then put it
//! where the chosen apps look for it.

use crate::db::{self, Db, PROVIDER_HF};
use crate::fetch::{Expect, Fetcher, Progress, Unavailable};
use crate::hub::{HubClient, Snapshot, TreeEntry};
use crate::ollama;
use crate::place::{self, App, AppDirs, Dest, Item, Placed};
use crate::select::{self, Selection};
use crate::store::Store;
use anyhow::{Context, Result, anyhow, bail};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Everything an operation needs. Shared by the command line and the daemon.
pub struct Engine {
    pub db: Mutex<Db>,
    pub store: Store,
    pub hub: HubClient,
    pub fetcher: Fetcher,
    pub dirs: AppDirs,
}

impl Engine {
    pub fn db(&self) -> std::sync::MutexGuard<'_, Db> {
        db::lock(&self.db)
    }
}

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

#[derive(Debug, Clone, serde::Serialize)]
pub struct PullOutcome {
    pub repo: String,
    pub commit: String,
    /// The provider could not be asked or no longer has the repository, and
    /// the archived copy was used.
    pub from_archive: bool,
    pub files: usize,
    pub bytes: u64,
    /// Files that had to be downloaded in this run.
    pub downloaded: usize,
    pub placed: Vec<(App, Placed)>,
    /// Things that did not stop the pull but the owner should know.
    pub notes: Vec<String>,
}

/// The revision a pull works on.
struct Resolved {
    repo_id: i64,
    snap: Snapshot,
    /// The provider could not answer and the archived copy is used.
    from_archive: bool,
    /// Why the archived copy is used, for the owner.
    note: Option<String>,
}

/// Find the revision: ask the provider, and fall back to the archive when it
/// cannot answer or no longer has the repository.
async fn resolve(engine: &Engine, req: &PullRequest) -> Result<Resolved> {
    let mut upstream_error = None;
    let fresh = if req.offline {
        None
    } else {
        match engine.hub.snapshot(&req.repo, &req.revision).await {
            Ok(s) => Some(s),
            Err(e) => {
                upstream_error = Some(e);
                None
            }
        }
    };
    let mut db = engine.db();
    if let Some(snap) = fresh {
        let repo_id = db.archive_snapshot(PROVIDER_HF, &req.repo, &req.revision, &snap)?;
        return Ok(Resolved {
            repo_id,
            snap,
            from_archive: false,
            note: None,
        });
    }
    match (
        db.archived(PROVIDER_HF, &req.repo, &req.revision)?,
        upstream_error,
    ) {
        (Some((repo_id, snap)), e) => Ok(Resolved {
            repo_id,
            snap,
            from_archive: true,
            note: e.map(|e| format!("{e}; using the archived copy")),
        }),
        (None, Some(e)) => bail!("{e} and it is not archived"),
        (None, None) => bail!(
            "{}@{} is not archived, and --offline was given",
            req.repo,
            req.revision
        ),
    }
}

/// Run blocking store work (a file may be copied back from the archive disk)
/// off the async runtime.
async fn blocking<T: Send + 'static>(
    engine: &Arc<Engine>,
    work: impl FnOnce(&Engine) -> Result<T> + Send + 'static,
) -> Result<T> {
    let engine = engine.clone();
    tokio::task::spawn_blocking(move || work(&engine)).await?
}

/// Fetch whatever of `entries` the store does not hold yet. Returns one item
/// per entry and the number downloaded.
async fn fetch_entries(
    engine: &Arc<Engine>,
    req: &PullRequest,
    commit: &str,
    entries: Vec<TreeEntry>,
    progress: &Arc<Progress>,
) -> Result<(Vec<Item>, usize)> {
    let mut items = Vec::new();
    let mut missing = Vec::new();
    for e in entries {
        let known = engine.db().blob_for_entry(&e)?;
        let Some(b) = known else {
            missing.push(e);
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
            progress.done.fetch_add(b.size, Ordering::Relaxed);
            items.push(Item {
                rel_path: e.path.clone(),
                sha256: b.sha256,
                size: b.size,
                etag: e.etag().to_string(),
            });
        } else {
            missing.push(e);
        }
    }
    if missing.is_empty() {
        return Ok((items, 0));
    }
    if req.offline {
        bail!(
            "{} files are not in the store, and --offline was given (first: {})",
            missing.len(),
            missing[0].path
        );
    }

    let downloaded = missing.len();
    let limit = Arc::new(Semaphore::new(req.jobs.max(1)));
    let mut running: JoinSet<Result<Item>> = JoinSet::new();
    for e in missing {
        let (engine, limit, progress) = (engine.clone(), limit.clone(), progress.clone());
        let url = engine.fetcher.resolve_url(&req.repo, commit, &e.path);
        running.spawn(async move {
            let _permit = limit.acquire_owned().await.expect("semaphore is open");
            let got = engine
                .fetcher
                .fetch(
                    &engine.store,
                    &url,
                    Some(e.size),
                    &e.expect(),
                    Some(&progress),
                )
                .await
                .with_context(|| format!("fetching {}", e.path))?;
            engine
                .db()
                .upsert_blob(&got.sha256, got.size, got.git_oid.as_deref())?;
            Ok(Item {
                etag: e.etag().to_string(),
                rel_path: e.path,
                sha256: got.sha256,
                size: got.size,
            })
        });
    }
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

/// Get the manifest Hugging Face's registry generates for `repo:tag`, archive
/// it, and make sure every layer it names is in the store. Returns the
/// manifest with its body as the registry sent it.
pub async fn ollama_manifest(
    engine: &Arc<Engine>,
    repo_id: i64,
    repo: &str,
    tag: &str,
    tree: &[TreeEntry],
    commit: &str,
    offline: bool,
) -> Result<(ollama::Manifest, String)> {
    let mut upstream_error = None;
    let mut fetched = None;
    if !offline {
        let url = format!("{}/v2/{repo}/manifests/{tag}", engine.fetcher.endpoint());
        let sent = engine
            .fetcher
            .get(&url)
            .header(reqwest::header::USER_AGENT, ollama::USER_AGENT)
            .header(reqwest::header::ACCEPT, ollama::MANIFEST_TYPE)
            .send()
            .await;
        match sent {
            Ok(resp) if resp.status().is_success() => {
                let media_type = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or(ollama::MANIFEST_TYPE)
                    .to_string();
                let text = resp.text().await?;
                let manifest = ollama::Manifest::parse(&text)?;
                engine
                    .db()
                    .save_manifest(repo_id, tag, ollama::USER_AGENT, &media_type, &text)?;
                fetched = Some((manifest, text));
            }
            Ok(resp) => {
                upstream_error = Some(anyhow!(
                    "Hugging Face has no Ollama manifest for {repo}:{tag} (HTTP {})",
                    resp.status().as_u16()
                ))
            }
            Err(e) => upstream_error = Some(anyhow!("could not reach Hugging Face: {e}")),
        }
    }
    let (manifest, body) = match fetched {
        Some(found) => found,
        None => {
            let archived = engine
                .db()
                .manifest(repo_id, tag, Some(ollama::USER_AGENT))?;
            match archived {
                Some((_, body)) => (ollama::Manifest::parse(&body)?, body),
                None => {
                    return Err(upstream_error.unwrap_or_else(|| {
                        anyhow!("no Ollama manifest for {repo}:{tag} is archived")
                    }));
                }
            }
        }
    };

    for layer in manifest.all() {
        let sha = layer.sha256()?;
        let owned = sha.to_string();
        if blocking(engine, move |engine| engine.store.ensure_local(&owned)).await? {
            if engine.db().blob(sha)?.is_none() {
                let size = std::fs::metadata(engine.store.blob_path(sha))?.len();
                engine.db().upsert_blob(sha, size, None)?;
            }
            continue;
        }
        if offline {
            bail!("layer {} of {repo}:{tag} is not in the store", layer.digest);
        }
        // A layer that is a file of the repository comes from the repository;
        // the layers Hugging Face generates exist only in its registry.
        let url = match tree.iter().find(|e| e.lfs_sha256.as_deref() == Some(sha)) {
            Some(e) => engine.fetcher.resolve_url(repo, commit, &e.path),
            None => engine.fetcher.registry_blob_url(repo, sha),
        };
        let size = (layer.size > 0).then_some(layer.size);
        let got = engine
            .fetcher
            .fetch(
                &engine.store,
                &url,
                size,
                &Expect::Sha256(sha.to_string()),
                None,
            )
            .await
            .with_context(|| format!("fetching layer {}", layer.digest))?;
        engine.db().upsert_blob(&got.sha256, got.size, None)?;
    }
    Ok((manifest, body))
}

/// Place the selection into Ollama. The inner `Err` is the reason it was not
/// placed, which does not fail the pull.
async fn place_into_ollama(
    engine: &Arc<Engine>,
    req: &PullRequest,
    repo_id: i64,
    snap: &Snapshot,
    entries: &[TreeEntry],
) -> Result<Result<Placed, String>> {
    let weights = select::weights(entries);
    if weights.len() != 1 {
        return Ok(Err(if weights.is_empty() {
            "not placed into Ollama: it only runs GGUF files from Hugging Face".into()
        } else {
            "not placed into Ollama: it cannot use a GGUF that is split into parts".into()
        }));
    }
    let tag = req
        .selection
        .quant
        .as_deref()
        .unwrap_or(ollama::DEFAULT_TAG);
    let found = ollama_manifest(
        engine,
        repo_id,
        &req.repo,
        tag,
        &snap.entries,
        &snap.commit,
        req.offline,
    )
    .await;
    let (manifest, body) = match found {
        Ok(m) => m,
        Err(e) => return Ok(Err(format!("not placed into Ollama: {e:#}"))),
    };
    // Without a named quantization the tag is Hugging Face's choice, which
    // need not be the file the patterns selected.
    let wanted = weights[0].lfs_sha256.as_deref();
    if !manifest.all().any(|l| l.sha256().ok() == wanted) {
        return Ok(Err(format!(
            "not placed into Ollama: its tag {tag} is a different file than {}; \
             name the quantization, as in {}:Q8_0",
            weights[0].path, req.repo
        )));
    }
    let (repo, tag) = (req.repo.clone(), tag.to_string());
    let placed = blocking(engine, move |engine| {
        place::place_ollama(&dest(engine, repo_id, &repo), &tag, &manifest, &body)
    })
    .await?;
    Ok(Ok(placed))
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

/// Fetch a revision into the store and place it into `req.apps`.
pub async fn pull(
    engine: &Arc<Engine>,
    req: &PullRequest,
    progress: &Arc<Progress>,
) -> Result<PullOutcome> {
    let Resolved {
        repo_id,
        snap,
        from_archive,
        note,
    } = resolve(engine, req).await?;
    let mut notes: Vec<String> = note.into_iter().collect();
    let entries = select::select(&snap.entries, &req.selection)?;
    let bytes: u64 = entries.iter().map(|e| e.size).sum();
    progress.total.fetch_add(bytes, Ordering::Relaxed);

    let (items, downloaded) =
        match fetch_entries(engine, req, &snap.commit, entries.clone(), progress).await {
            Ok(v) => v,
            Err(e) if from_archive && e.downcast_ref::<Unavailable>().is_some() => {
                return Err(e.context(format!(
                    "{} is gone upstream and this file was never archived",
                    req.repo
                )));
            }
            Err(e) => return Err(e),
        };

    // Placement hashes and copies files, so it runs off the async runtime.
    let items = Arc::new(items);
    let mut placed = Vec::new();
    for app in &req.apps {
        let (repo, commit, items) = (req.repo.clone(), snap.commit.clone(), items.clone());
        let result = match app {
            App::Bionic => {
                blocking(engine, move |engine| {
                    place::place_bionic(&dest(engine, repo_id, &repo), &commit, &items)
                })
                .await?
            }
            App::Hf => {
                let ref_name = db::ref_name(&req.revision).map(str::to_string);
                blocking(engine, move |engine| {
                    place::place_hf(
                        &dest(engine, repo_id, &repo),
                        &commit,
                        ref_name.as_deref(),
                        &items,
                    )
                })
                .await?
            }
            App::Ollama => match place_into_ollama(engine, req, repo_id, &snap, &entries).await? {
                Ok(result) => result,
                Err(note) => {
                    notes.push(note);
                    continue;
                }
            },
        };
        for c in &result.conflicts {
            notes.push(format!("left alone: {c}"));
        }
        placed.push((*app, result));
    }

    Ok(PullOutcome {
        repo: req.repo.clone(),
        commit: snap.commit,
        from_archive,
        files: items.len(),
        bytes,
        downloaded,
        placed,
        notes,
    })
}

/// Split `org/repo[:quant][@revision]` as typed on the command line.
pub fn parse_model(spec: &str) -> Result<(String, Option<String>, String)> {
    let spec = spec
        .trim()
        .trim_start_matches("https://huggingface.co/")
        .trim_start_matches("hf.co/");
    let (rest, revision) = match spec.split_once('@') {
        Some((r, rev)) => (r, rev.to_string()),
        None => (spec, "main".to_string()),
    };
    let (repo, quant) = match rest.split_once(':') {
        Some((r, q)) => (r, Some(q.to_string())),
        None => (rest, None),
    };
    let ok = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    let parts: Vec<&str> = repo.split('/').collect();
    if parts.len() != 2 || !parts.iter().all(|p| ok(p)) || revision.is_empty() {
        bail!("{spec} is not a model name; use org/repo, org/repo:Q4_K_M or org/repo@revision");
    }
    Ok((repo.to_string(), quant, revision))
}
