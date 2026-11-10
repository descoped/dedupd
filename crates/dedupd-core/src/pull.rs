//! `pull` and `place`: get a repository revision into the store, then put it
//! where the chosen apps look for it.

use crate::adopt::PROVIDER_HF;
use crate::db::{Db, is_commit};
use crate::fetch::{Expect, Fetcher, Progress, Unavailable};
use crate::hub::{HubClient, Snapshot, TreeEntry};
use crate::ollama;
use crate::place::{self, App, AppDirs, Item, Placed};
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
        self.db.lock().expect("database lock")
    }
}

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

/// Find the revision: ask the provider, and fall back to the archive when it
/// cannot answer or no longer has the repository.
async fn resolve(engine: &Engine, req: &PullRequest) -> Result<(i64, Snapshot, bool, Vec<String>)> {
    let mut notes = Vec::new();
    let fresh = if req.offline {
        None
    } else {
        match engine.hub.snapshot(&req.repo, &req.revision).await {
            Ok(s) => Some(s),
            Err(e) => {
                notes.push(format!("{e}; using the archived copy"));
                None
            }
        }
    };
    let mut db = engine.db();
    match fresh {
        Some(snap) => {
            let repo_id = db.upsert_repo(PROVIDER_HF, &req.repo)?;
            let ref_name = (!is_commit(&req.revision)).then_some(req.revision.as_str());
            db.record_snapshot(repo_id, ref_name, &snap)?;
            Ok((repo_id, snap, false, notes))
        }
        None => {
            let archived = match db.repo_id(PROVIDER_HF, &req.repo)? {
                Some(id) => db.archived_snapshot(id, &req.revision)?.map(|s| (id, s)),
                None => None,
            };
            match archived {
                Some((id, snap)) => Ok((id, snap, true, notes)),
                None if req.offline => bail!(
                    "{}@{} is not archived, and --offline was given",
                    req.repo,
                    req.revision
                ),
                None => bail!(
                    "{} and it is not archived",
                    notes
                        .pop()
                        .unwrap_or_default()
                        .replace("; using the archived copy", "")
                ),
            }
        }
    }
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
        // A copy that cannot be brought back from the archive disk (detached
        // mid-copy, damaged) is downloaded again when that is allowed.
        let local = |sha: &str| match engine.store.ensure_local(sha) {
            Err(e) if req.offline => Err(e),
            other => Ok(other.unwrap_or(false)),
        };
        match known {
            Some(b) if local(&b.sha256)? => {
                progress.done.fetch_add(b.size, Ordering::Relaxed);
                items.push(Item {
                    rel_path: e.path.clone(),
                    sha256: b.sha256,
                    size: b.size,
                    etag: e.lfs_sha256.clone().unwrap_or(e.git_oid.clone()),
                });
            }
            _ => missing.push(e),
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
            let expect = match &e.lfs_sha256 {
                Some(sha) => Expect::Sha256(sha.clone()),
                None => Expect::GitOid(e.git_oid.clone()),
            };
            let got = engine
                .fetcher
                .fetch(&engine.store, &url, Some(e.size), &expect, Some(&progress))
                .await
                .with_context(|| format!("fetching {}", e.path))?;
            engine
                .db()
                .upsert_blob(&got.sha256, got.size, got.git_oid.as_deref())?;
            Ok(Item {
                rel_path: e.path.clone(),
                sha256: got.sha256,
                size: got.size,
                etag: e.lfs_sha256.unwrap_or(e.git_oid),
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
/// it, and make sure every layer it names is in the store.
pub async fn ollama_manifest(
    engine: &Engine,
    repo_id: i64,
    repo: &str,
    tag: &str,
    tree: &[TreeEntry],
    commit: &str,
    offline: bool,
) -> Result<String> {
    let mut upstream_error = None;
    let mut body = None;
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
                ollama::Manifest::parse(&text)?;
                engine
                    .db()
                    .save_manifest(repo_id, tag, ollama::USER_AGENT, &media_type, &text)?;
                body = Some(text);
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
    let body = match body {
        Some(b) => b,
        None => match engine
            .db()
            .manifest(repo_id, tag, Some(ollama::USER_AGENT))?
        {
            Some((_, b)) => b,
            None => {
                return Err(upstream_error.unwrap_or_else(|| {
                    anyhow!("no Ollama manifest for {repo}:{tag} is archived")
                }));
            }
        },
    };

    let manifest = ollama::Manifest::parse(&body)?;
    for layer in manifest.all() {
        let sha = layer.sha256()?;
        if engine.store.ensure_local(sha)? {
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
    Ok(body)
}

/// Fetch a revision into the store and place it into `req.apps`.
pub async fn pull(
    engine: &Arc<Engine>,
    req: &PullRequest,
    progress: &Arc<Progress>,
) -> Result<PullOutcome> {
    let (repo_id, snap, from_archive, mut notes) = resolve(engine, req).await?;
    let entries = select::select(&snap.entries, &req.selection)?;
    let bytes: u64 = entries.iter().map(|e| e.size).sum();
    progress.total.fetch_add(bytes, Ordering::Relaxed);

    // With the provider out of reach, whatever is missing cannot be fetched.
    let fetch_req = PullRequest {
        offline: req.offline,
        ..req.clone()
    };
    let (items, downloaded) =
        match fetch_entries(engine, &fetch_req, &snap.commit, entries.clone(), progress).await {
            Ok(v) => v,
            Err(e) if from_archive && e.downcast_ref::<Unavailable>().is_some() => {
                return Err(e.context(format!(
                    "{} is gone upstream and this file was never archived",
                    req.repo
                )));
            }
            Err(e) => return Err(e),
        };

    let ref_name = (!is_commit(&req.revision)).then_some(req.revision.as_str());
    let mut placed = Vec::new();
    for app in &req.apps {
        let result = match app {
            App::Bionic => place::place_bionic(
                &engine.db(),
                &engine.store,
                &engine.dirs,
                repo_id,
                &req.repo,
                &snap.commit,
                &items,
            )?,
            App::Hf => place::place_hf(
                &engine.db(),
                &engine.store,
                &engine.dirs,
                repo_id,
                &req.repo,
                &snap.commit,
                ref_name,
                &items,
            )?,
            App::Ollama => {
                let weights = select::weights(&entries);
                if weights.len() != 1 {
                    notes.push(if weights.is_empty() {
                        "not placed into Ollama: it only runs GGUF files from Hugging Face".into()
                    } else {
                        "not placed into Ollama: it cannot use a GGUF that is split into parts"
                            .into()
                    });
                    continue;
                }
                let tag = req
                    .selection
                    .quant
                    .as_deref()
                    .unwrap_or(ollama::DEFAULT_TAG);
                let manifest = ollama_manifest(
                    engine,
                    repo_id,
                    &req.repo,
                    tag,
                    &snap.entries,
                    &snap.commit,
                    req.offline,
                )
                .await;
                let manifest = match manifest {
                    Ok(m) => m,
                    Err(e) => {
                        notes.push(format!("not placed into Ollama: {e:#}"));
                        continue;
                    }
                };
                // Without a named quantization the tag is Hugging Face's
                // choice, which need not be the file the patterns selected.
                let wanted = weights[0].lfs_sha256.as_deref();
                if !manifest.all().any(|l| l.sha256().ok() == wanted) {
                    notes.push(format!(
                        "not placed into Ollama: its tag {tag} is a different file than {}; \
                         name the quantization, as in {}:Q8_0",
                        weights[0].path, req.repo
                    ));
                    continue;
                }
                place::place_ollama(
                    &engine.db(),
                    &engine.store,
                    &engine.dirs,
                    repo_id,
                    &req.repo,
                    tag,
                    &manifest,
                )?
            }
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
