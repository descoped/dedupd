//! `pull` and `place`: get a repository revision into the store, then put it
//! where the chosen apps look for it.

use crate::db::{self, Db, PROVIDER_HF};
use crate::fetch::{Expect, Fetcher, Progress, Unavailable, Unwatched, Wait, Watch};
use crate::governor::Governor;
use crate::hub::{HubClient, ModelMeta, Snapshot, TreeEntry};
use crate::ollama;
use crate::ollama_mlx;
use crate::place::{self, App, AppDirs, Dest, Item, Placed};
use crate::profile::{self, Installed, Traits};
use crate::refusals::{Refusal, Refusals};
use crate::select::{self, Selection};
use crate::store::Store;
use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;
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
    /// Paces every request to the provider; shared by `hub` and `fetcher`.
    pub governor: Arc<Governor>,
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

/// Run blocking store work (a file may be copied back from the archive disk)
/// off the async runtime.
async fn blocking<T: Send + 'static>(
    engine: &Arc<Engine>,
    work: impl FnOnce(&Engine) -> Result<T> + Send + 'static,
) -> Result<T> {
    let engine = engine.clone();
    tokio::task::spawn_blocking(move || work(&engine)).await?
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
                &Unwatched,
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
    items: &Arc<Vec<Item>>,
) -> Result<Result<Placed, String>> {
    // The app's profile says whether this selection can go in, and why not.
    let weights = select::weights(entries);
    let info: serde_json::Value = serde_json::from_str(&snap.info_json).unwrap_or_default();
    let mut traits = Traits::of_files(weights.len(), &ModelMeta::from_info(&info).formats);
    let config = items
        .iter()
        .find(|i| i.rel_path == "config.json")
        .map(|i| i.sha256.clone());
    if weights.is_empty() {
        traits = traits.with_bits(
            config
                .as_deref()
                .and_then(|sha| profile::quantization_of(&engine.store.blob_path(sha))),
        );
    }
    let installed = Installed::detect(&engine.dirs);
    let decision = profile::profile(App::Ollama)
        .decide(&traits, installed.version(App::Ollama))
        .unless_refused(&Refusals::load(engine.store.home()), config.as_deref());
    if decision.state != "ready" {
        return Ok(Err(format!("not placed into Ollama: {}", decision.reason)));
    }
    if decision.method.as_deref() == Some(ollama_mlx::METHOD) {
        let Some(command) = installed.command(App::Ollama).map(Path::to_path_buf) else {
            return Ok(Err(
                "not placed into Ollama: its command line tool was not found".into(),
            ));
        };
        let (repo, items) = (req.repo.clone(), items.clone());
        let placed = blocking(engine, move |engine| {
            ollama_mlx::place(
                &dest(engine, repo_id, &repo),
                &command,
                ollama::DEFAULT_TAG,
                &items,
            )
        })
        .await?;
        if let (Err(ollama_mlx::NotPlaced::Ollama(why)), Some(version), Some(config)) =
            (&placed, installed.version(App::Ollama), config)
        {
            // Ollama's answer holds for this model and this version; keep it,
            // so the model is not offered to it again.
            let refusal = Refusal {
                app: App::Ollama.as_str().to_string(),
                version: version.to_string(),
                method: ollama_mlx::METHOD.to_string(),
                config,
                reason: why.clone(),
                at: crate::now(),
            };
            if let Err(e) = Refusals::record(engine.store.home(), refusal) {
                eprintln!("dedupd: could not keep Ollama's answer: {e:#}");
            }
        }
        return Ok(placed.map_err(|why| format!("not placed into Ollama: {why}")));
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
) -> Result<(Vec<(App, Placed)>, Vec<String>)> {
    let (repo_id, snap) = (plan.repo_id, &plan.snap);
    // The files are in the store again, so the model is no longer archived.
    engine.db().set_archived(repo_id, None)?;
    let mut notes = Vec::new();
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
            App::Ollama => {
                match place_into_ollama(engine, req, repo_id, snap, &plan.entries, &items).await? {
                    Ok(result) => result,
                    Err(note) => {
                        notes.push(note);
                        continue;
                    }
                }
            }
        };
        for c in &result.conflicts {
            notes.push(format!("left alone: {c}"));
        }
        placed.push((*app, result));
    }
    Ok((placed, notes))
}

/// Fetch a revision into the store and place it into `req.apps`.
pub async fn pull(
    engine: &Arc<Engine>,
    req: &PullRequest,
    progress: &Arc<Progress>,
) -> Result<PullOutcome> {
    let plan = plan(engine, req, None).await?;
    progress.total.fetch_add(plan.bytes, Ordering::Relaxed);
    let (items, downloaded) = fetch(engine, req, &plan, progress.clone())
        .await
        .map_err(|e| gone_upstream(e, req, &plan))?;
    let files = items.len();
    let (placed, notes) = place(engine, req, &plan, items).await?;
    Ok(outcome(req, plan, files, downloaded, placed, notes))
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
    placed: Vec<(App, Placed)>,
    notes: Vec<String>,
) -> PullOutcome {
    PullOutcome {
        repo: req.repo.clone(),
        commit: plan.snap.commit,
        from_archive: plan.from_archive,
        files,
        bytes: plan.bytes,
        downloaded,
        placed,
        notes: plan.note.into_iter().chain(notes).collect(),
    }
}

/// One half of `org/name`, by Hugging Face's own rule: up to 96 letters,
/// digits, `-`, `_` and `.`; it starts and ends with a letter, a digit or
/// `_`; no `--`, no `..`, and it does not end in `.git`.
fn is_name_part(part: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    !part.is_empty()
        && part.len() <= 96
        && part.starts_with(word)
        && part.ends_with(word)
        && part.chars().all(|c| word(c) || c == '-' || c == '.')
        && !part.contains("--")
        && !part.contains("..")
        && !part.ends_with(".git")
}

/// Whether the text is the name of a repository, `org/name`, and nothing
/// else. A name that came from outside (a request, a pasted address) is
/// checked with this before it is put into a path or sent to the provider:
/// such a name can hold nothing that climbs out of a folder or changes the
/// meaning of an address.
pub fn is_repo_name(repo: &str) -> bool {
    let mut parts = repo.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(org), Some(name), None) if is_name_part(org) && is_name_part(name)
    )
}

/// Split `org/repo[:quant][@revision]` as typed on the command line. Every
/// part is checked: a quantization is a short word, and a revision is a
/// branch, tag or commit with nothing in it that climbs out of a path.
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
    let quant_ok = quant.as_deref().is_none_or(|q| {
        !q.is_empty()
            && q.len() <= 64
            && !q.contains("..")
            && q.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    });
    let revision_ok = !revision.is_empty()
        && revision.len() <= 200
        && !revision.contains("..")
        && !revision.contains("//")
        && !revision.starts_with(['/', '-', '.'])
        && !revision.ends_with('/')
        && revision
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c));
    if !is_repo_name(repo) || !quant_ok || !revision_ok {
        bail!("{spec} is not a model name; use org/repo, org/repo:Q4_K_M or org/repo@revision");
    }
    Ok((repo.to_string(), quant, revision))
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn a_model_name_is_read_and_checked() {
        let ok = |spec: &str| parse_model(spec).unwrap();
        assert_eq!(ok("org/repo"), ("org/repo".into(), None, "main".into()));
        assert_eq!(
            ok(" https://huggingface.co/org_x/Repo-1.5_b:Q4_K_M@refs/pr/3 "),
            (
                "org_x/Repo-1.5_b".into(),
                Some("Q4_K_M".into()),
                "refs/pr/3".into()
            )
        );
        assert_eq!(ok("hf.co/org/repo@v1.0").2, "v1.0");

        for hostile in [
            "",
            "org",
            "org/repo/extra",
            "org/..",
            "../etc/passwd",
            "org/re..po",
            "org/re--po",
            "org/-repo",
            "org/repo.",
            "org/repo.git",
            "org/re po",
            "org/repo;rm",
            "org/$(id)",
            "org/re%2Fpo",
            "org/repo:../../x",
            "org/repo:Q4/../x",
            "org/repo:",
            "org/repo@",
            "org/repo@../../main",
            "org/repo@/etc",
            "org/repo@main//x",
            "org/repo@-x",
            "org/repo@main?x=1",
            "https://evil.example/org/repo",
            "org/rep\u{43e}",
        ] {
            assert!(parse_model(hostile).is_err(), "{hostile:?} was taken");
        }
        assert!(is_repo_name(&format!("org/{}", "a".repeat(96))));
        assert!(!is_repo_name(&format!("org/{}", "a".repeat(97))));
        assert!(!is_repo_name("org/repo:Q4_K_M"));
        assert!(!is_repo_name("org/repo@main"));
    }
}
