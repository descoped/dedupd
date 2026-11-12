//! The control API under `/dedupd/v1`, for the command line and the app.
//!
//! Everything the app shows comes from here, and everything it does goes
//! through here: the daemon owns the store, the database and the downloads.

use crate::moves::{self, Kind};
use crate::{Call, Server, daemon, json, percent_decode, plain};
use axum::body::{Body, Bytes};
use axum::http::{Method, StatusCode, header};
use axum::response::Response;
use dedupd_core::config::Config;
use dedupd_core::db::PROVIDER_HF;
use dedupd_core::fetch::encode_path;
use dedupd_core::hub::{HubClient, HubError};
use dedupd_core::place::App;
use dedupd_core::profile::Installed;
use dedupd_core::queue::{FileStage, JobRecord, State};
use dedupd_core::{archive, library, maintain, status};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json as j};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// A line goes out on the event stream at least this often, so the reader
/// can tell a quiet daemon from a dead connection.
const HEARTBEAT: Duration = Duration::from_secs(15);
/// After a change, wait this long before telling, so a burst of changes is
/// one line and progress costs the reader little.
const SETTLE: Duration = Duration::from_millis(350);
/// The most of the log that is read for its last lines.
const LOG_TAIL: u64 = 512 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PullBody {
    model: String,
    #[serde(default)]
    apps: Vec<App>,
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    offline: bool,
    /// Accepted from older clients; the daemon decides how many downloads
    /// run at once.
    #[serde(default, rename = "jobs")]
    _jobs: Option<usize>,
}

#[derive(Deserialize)]
struct RepoBody {
    repo: String,
}

#[derive(Deserialize)]
struct MoveBody {
    repo: String,
    /// Do it. Without it the answer only says what would happen.
    #[serde(default)]
    apply: bool,
}

#[derive(Deserialize)]
struct UnplaceBody {
    repo: String,
    app: App,
    /// Remove the files. Without it the answer only says what would go.
    #[serde(default)]
    apply: bool,
    /// The files the owner agreed to; nothing else is removed.
    paths: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct GcBody {
    /// sha256 of the unreferenced files the owner agreed to remove.
    #[serde(default)]
    orphans: Vec<String>,
    /// sha256 of the files whose local copy the owner agreed to remove.
    #[serde(default)]
    offload: Vec<String>,
}

pub(crate) fn error(status: StatusCode, e: impl std::fmt::Display) -> Response {
    json(status, &j!({"error": e.to_string()}))
}

fn internal(e: impl std::fmt::Display) -> Response {
    error(StatusCode::INTERNAL_SERVER_ERROR, e)
}

pub(crate) fn ok(value: impl serde::Serialize) -> Response {
    json(
        StatusCode::OK,
        &serde_json::to_value(value).unwrap_or_default(),
    )
}

fn answer<T: serde::Serialize>(result: anyhow::Result<T>) -> Response {
    match result {
        Ok(value) => ok(value),
        Err(e) => internal(format!("{e:#}")),
    }
}

/// What a handler answers instead of doing its work.
type Refused = Box<Response>;

fn body<T: DeserializeOwned>(call: &Call) -> Result<T, Refused> {
    serde_json::from_slice(&call.body).map_err(|e| Box::new(error(StatusCode::BAD_REQUEST, e)))
}

/// What the provider's failure means for someone who asked the daemon.
pub(crate) fn hub_error(e: HubError) -> Response {
    let status = match &e {
        HubError::NotAvailable { .. } => StatusCode::NOT_FOUND,
        HubError::Busy { .. } => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::BAD_GATEWAY,
    };
    error(status, e)
}

/// Run work that reads files or waits for the database off the threads that
/// answer requests.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(work).await?
}

/// The launchd label this process runs under; `None` when it was started
/// by hand.
fn launchd_label() -> Option<String> {
    let name = std::env::var("XPC_SERVICE_NAME").ok()?;
    let by_launchd = std::os::unix::process::parent_id() == 1;
    (by_launchd && name != "0" && !name.starts_with("application.")).then_some(name)
}

fn status_answer(server: &Server) -> anyhow::Result<Value> {
    let s = status::status(&server.engine.db())?;
    let archive_home = server.config.archive_home();
    let (total, done) = server.queue.totals();
    let governor = server.engine.governor.state();
    Ok(j!({
        "version": env!("CARGO_PKG_VERSION"),
        "home": server.home.root(),
        "endpoint": server.engine.fetcher.endpoint(),
        "archive": archive_home.as_ref().map(|a| j!({
            "path": a.root(),
            "attached": archive::attached(a),
        })),
        "downloading": {"total": total, "done": done},
        "daemon": {
            "pid": std::process::id(),
            "started_at": server.started_at,
            "listen": server.config.listen(),
            "executable": std::env::current_exe().ok(),
            "managed_by": launchd_label(),
            "token": server.engine.hub.has_token(),
            "token_source": HubClient::token_source(),
            "max_connections": governor.max_connections,
            "speed_limit": governor.speed_limit,
            "checking": server.check.lock().expect("lock").running,
            "last_check_at": daemon::last_check(server),
        },
        "status": s,
    }))
}

/// A job without its file list and history, which only the job's own page
/// needs.
fn job_summary(record: &JobRecord) -> Value {
    let mut value = serde_json::to_value(record).unwrap_or_default();
    if let Some(o) = value.as_object_mut() {
        for key in ["files", "events", "outcome"] {
            o.remove(key);
        }
        let stored = record
            .files
            .iter()
            .filter(|f| f.stage == FileStage::Stored)
            .count();
        o.insert("files_total".into(), j!(record.files.len()));
        o.insert("files_done".into(), j!(stored));
    }
    value
}

fn downloads(server: &Server) -> Value {
    let snapshot = server.queue.snapshot();
    j!({
        "governor": snapshot.governor,
        "speed": snapshot.speed,
        "jobs": snapshot.jobs.iter().map(job_summary).collect::<Vec<_>>(),
    })
}

/// A stream of lines, one JSON object each, that says what the daemon is
/// doing: one line now, then one whenever something changes. The reader
/// keeps the latest line and needs nothing else to show progress.
fn events(server: &Arc<Server>) -> Response {
    struct Feed {
        server: Arc<Server>,
        queue: tokio::sync::watch::Receiver<u64>,
        library: tokio::sync::watch::Receiver<u64>,
        moving: tokio::sync::watch::Receiver<Option<moves::MoveState>>,
        seq: u64,
    }
    let feed = Feed {
        server: server.clone(),
        queue: server.queue.subscribe(),
        library: server.library_changes(),
        moving: server.moving.subscribe(),
        seq: 0,
    };
    let lines = futures_util::stream::unfold(feed, |mut feed| async move {
        if feed.seq > 0 {
            tokio::select! {
                _ = feed.server.shutdown.cancelled() => return None,
                _ = feed.queue.changed() => {}
                _ = feed.library.changed() => {}
                _ = feed.moving.changed() => {}
                _ = tokio::time::sleep(HEARTBEAT) => {}
            }
            tokio::select! {
                _ = feed.server.shutdown.cancelled() => return None,
                _ = tokio::time::sleep(SETTLE) => {}
            }
        }
        feed.seq += 1;
        feed.queue.borrow_and_update();
        let library = *feed.library.borrow_and_update();
        let moving = feed.moving.borrow_and_update().clone();
        let mut line = j!({
            "seq": feed.seq,
            "library": library,
            "moving": moving,
            "checking": feed.server.check.lock().expect("lock").running,
            "downloads": downloads(&feed.server),
        })
        .to_string();
        line.push('\n');
        Some((Ok::<_, std::convert::Infallible>(Bytes::from(line)), feed))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(lines))
        .expect("valid response")
}

/// Queue a download, or name the one already queued for the same thing.
fn enqueue(server: &Arc<Server>, body: PullBody) -> Result<(i64, bool), Refused> {
    let wanted: HashSet<App> = body.apps.iter().copied().collect();
    let same = server.queue.snapshot().jobs.into_iter().find(|job| {
        !job.state.is_finished()
            && job.model == body.model.trim()
            && job.include == body.include
            && job.offline == body.offline
            && job.apps.iter().copied().collect::<HashSet<_>>() == wanted
    });
    if let Some(job) = same {
        if matches!(job.state, State::Paused | State::NeedsAttention) {
            let _ = server.queue.resume(job.id);
        }
        return Ok((job.id, true));
    }
    server
        .queue
        .add(&body.model, body.include, body.apps, body.offline)
        .map(|id| (id, false))
        .map_err(|e| Box::new(error(StatusCode::BAD_REQUEST, e)))
}

async fn download_action(server: &Arc<Server>, id: &str, action: &str) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return error(StatusCode::BAD_REQUEST, "a download is named by its number");
    };
    let queue = &server.queue;
    let done = match action {
        "pause" => queue.pause(id).await,
        "resume" | "retry" => queue.resume(id),
        "start" => queue.start_now(id),
        "cancel" => queue.cancel(id).await,
        "remove" => queue.remove(id),
        _ => return plain(StatusCode::NOT_FOUND, "no such control endpoint"),
    };
    match done {
        Ok(()) => ok(j!({"id": id})),
        Err(e) => error(StatusCode::CONFLICT, e),
    }
}

/// Ask the provider whether it still has the repository and where `main`
/// is now. Nothing is downloaded.
async fn check_upstream(server: &Arc<Server>, repo: &str) -> Response {
    let engine = &server.engine;
    let known = {
        let db = engine.db();
        match db.repo_id(PROVIDER_HF, repo) {
            Ok(Some(id)) => db.ref_commit(id, "main").ok().flatten(),
            Ok(None) => {
                return error(
                    StatusCode::NOT_FOUND,
                    format!("{repo} is not in the library"),
                );
            }
            Err(e) => return internal(format!("{e:#}")),
        }
    };
    let path = format!("/api/models/{}/revision/main", encode_path(repo));
    let answer = match engine.hub.api(&path, repo).await {
        Ok(resp) => resp.json::<Value>().await.map_err(HubError::from),
        Err(e) => Err(e),
    };
    let found = match answer {
        Ok(info) => {
            let latest = info.get("sha").and_then(|v| v.as_str()).map(str::to_string);
            let _ = engine.db().set_upstream(PROVIDER_HF, repo, false);
            j!({
                "upstream": "ok",
                "commit": known,
                "latest": latest,
                "changed": latest.is_some() && latest != known,
            })
        }
        Err(HubError::NotAvailable { .. }) => {
            let _ = engine.db().set_upstream(PROVIDER_HF, repo, true);
            j!({"upstream": "gone", "commit": known, "latest": null, "changed": false})
        }
        Err(e) => return hub_error(e),
    };
    server.touch();
    ok(found)
}

async fn unplace(server: &Arc<Server>, body: UnplaceBody) -> Response {
    let engine = server.engine.clone();
    let done = blocking(move || {
        let mut plan = maintain::unplace_plan(
            &engine.db,
            &engine.store,
            &engine.dirs,
            &body.repo,
            body.app,
        )?;
        if let Some(agreed) = &body.paths {
            let agreed: HashSet<&str> = agreed.iter().map(String::as_str).collect();
            plan.remove
                .retain(|(path, _)| agreed.contains(path.as_str()));
            plan.bytes = plan.remove.iter().map(|(_, size)| size).sum();
        }
        let removed = match body.apply {
            true => Some(maintain::unplace_apply(&engine.db, &engine.dirs, &plan)?),
            false => None,
        };
        let pairs = |list: &[(String, String)]| -> Vec<Value> {
            list.iter()
                .map(|(path, why)| j!({"path": path, "why": why}))
                .collect()
        };
        Ok(j!({
            "repo": plan.repo,
            "app": plan.app,
            "remove": plan.remove.iter()
                .map(|(path, size)| j!({"path": path, "size": size}))
                .collect::<Vec<_>>(),
            "links": plan.links,
            "keep": pairs(&plan.keep),
            "bytes": plan.bytes,
            "removed": removed,
        }))
    })
    .await;
    if done.is_ok() {
        server.touch();
    }
    answer(done)
}

/// Archive or restore one model: say what it would do, or start doing it.
async fn move_model(server: &Arc<Server>, kind: Kind, body: MoveBody) -> Response {
    if body.apply {
        return match moves::start(server, kind, body.repo).await {
            Ok(()) => ok(j!({"started": true})),
            Err(why) => error(StatusCode::CONFLICT, why),
        };
    }
    let server = server.clone();
    match blocking(move || moves::plan(&server, kind, &body.repo)).await {
        Ok(plan) => ok(plan),
        Err(e) => error(StatusCode::CONFLICT, format!("{e:#}")),
    }
}

fn gc_answer(plan: &maintain::GcPlan, freed: Option<u64>) -> Value {
    let bytes = |list: &[dedupd_core::db::BlobRow]| list.iter().map(|b| b.size).sum::<u64>();
    j!({
        "orphans": plan.orphans,
        "offload": plan.offload,
        "orphan_bytes": bytes(&plan.orphans),
        "offload_bytes": bytes(&plan.offload),
        "freed": freed,
    })
}

/// The settings as they are in the config file, with the defaults filled
/// in, and whether the running daemon still uses older ones.
fn config_answer(server: &Server) -> anyhow::Result<Value> {
    let file = Config::load(&server.home)?;
    // An app folder the file does not name is where the daemon found it.
    let dirs = &server.engine.dirs;
    // The two download limits take effect at once; everything else is read
    // when the daemon starts.
    let fixed = |c: &Config| {
        let mut c = c.clone();
        (c.max_connections, c.speed_limit) = (None, None);
        serde_json::to_value(c).unwrap_or_default()
    };
    Ok(j!({
        "path": Config::path(&server.home),
        "listen": file.listen(),
        "hf_endpoint": file.hf_endpoint.as_deref().unwrap_or(dedupd_core::hub::DEFAULT_ENDPOINT),
        "archive": file.archive,
        "bionic_models": file.bionic_models.as_ref().unwrap_or(&dirs.bionic),
        "ollama_models": file.ollama_models.as_ref().unwrap_or(&dirs.ollama),
        "hf_cache": file.hf_cache.as_ref().unwrap_or(&dirs.hf_cache),
        "reclaim": file.reclaim != Some(false),
        "set_env": file.set_env == Some(true),
        "verify_days": file.verify_days.unwrap_or(7),
        "scan_interval": file.scan_interval.unwrap_or(300),
        "max_connections": file.max_connections(),
        "speed_limit": file.speed_limit(),
        "restart_needed": fixed(&file) != fixed(&server.config),
    }))
}

/// The last `lines` lines of the daemon's log.
fn log_tail(server: &Server, lines: usize) -> anyhow::Result<Value> {
    use std::io::{Read, Seek, SeekFrom};
    let path = server.home.root().join("daemon.log");
    let mut text = Vec::new();
    if let Ok(mut file) = std::fs::File::open(&path) {
        let size = file.metadata()?.len();
        file.seek(SeekFrom::Start(size.saturating_sub(LOG_TAIL)))?;
        file.read_to_end(&mut text)?;
    }
    let text = String::from_utf8_lossy(&text);
    let all: Vec<&str> = text.lines().collect();
    let from = all.len().saturating_sub(lines);
    Ok(j!({"path": path, "lines": all[from..]}))
}

pub(crate) async fn handle(server: &Arc<Server>, call: &Call, rest: &[&str]) -> Response {
    // A web page must not be able to drive the daemon: browsers send Origin
    // on cross-site requests, and no real client of this API does.
    if call.header(header::ORIGIN).is_some() {
        return plain(
            StatusCode::FORBIDDEN,
            "the control API is not for web pages",
        );
    }
    let engine = server.engine.clone();
    match (&call.method, rest) {
        (&Method::GET, ["status"]) => answer(status_answer(server)),
        (&Method::POST, ["pull"]) => {
            let body: PullBody = match body(call) {
                Ok(b) => b,
                Err(refused) => return *refused,
            };
            // A pull is a download like any other; this call waits for it,
            // which is what the command line wants.
            let id = match enqueue(server, body) {
                Ok((id, _)) => id,
                Err(refused) => return *refused,
            };
            match server.queue.settled(id).await {
                Ok(job) if job.state == State::Done => {
                    json(StatusCode::OK, &job.outcome.unwrap_or_default())
                }
                Ok(job) => error(
                    StatusCode::BAD_GATEWAY,
                    job.detail
                        .unwrap_or_else(|| format!("the download is {}", job.state.as_str())),
                ),
                Err(e) => internal(format!("{e:#}")),
            }
        }
        (&Method::POST, ["scan"]) => answer(daemon::pass(server).await),

        (&Method::GET, ["events"]) => events(server),
        (&Method::GET, ["downloads"]) => ok(downloads(server)),
        (&Method::POST, ["downloads"]) => {
            let body: PullBody = match body(call) {
                Ok(b) => b,
                Err(refused) => return *refused,
            };
            match enqueue(server, body) {
                Ok((id, existing)) => ok(j!({"id": id, "existing": existing})),
                Err(refused) => *refused,
            }
        }
        (&Method::POST, ["downloads", "clear"]) => match server.queue.clear_finished() {
            Ok(removed) => ok(j!({"removed": removed})),
            Err(e) => internal(format!("{e:#}")),
        },
        (&Method::GET, ["downloads", id]) => match id.parse().map(|id| server.queue.get(id)) {
            Ok(Ok(job)) => ok(job),
            _ => error(StatusCode::NOT_FOUND, format!("there is no download {id}")),
        },
        (&Method::POST, ["downloads", id, action]) => download_action(server, id, action).await,

        (&Method::GET, ["models"]) => {
            answer(blocking(move || Ok(j!({"models": library::models(&engine.db())?}))).await)
        }
        (&Method::GET, ["models", name @ ..]) if !name.is_empty() => {
            let repo = percent_decode(&name.join("/"));
            let found = {
                let repo = repo.clone();
                blocking(move || {
                    let installed = Installed::detect(&engine.dirs);
                    library::model(&engine.db(), &engine.store, &repo, &installed)
                })
                .await
            };
            match found {
                Ok(Some(detail)) => ok(detail),
                Ok(None) => error(
                    StatusCode::NOT_FOUND,
                    format!("{repo} is not in the library"),
                ),
                Err(e) => internal(format!("{e:#}")),
            }
        }
        (&Method::GET, ["locations"]) => answer(
            blocking(move || library::locations(&engine.db(), &engine.store, &engine.dirs)).await,
        ),

        (&Method::POST, ["check"]) => match body::<RepoBody>(call) {
            Ok(b) => check_upstream(server, &b.repo).await,
            Err(refused) => *refused,
        },
        (&Method::POST, ["forget"]) => match body::<RepoBody>(call) {
            Ok(b) => {
                let repo = b.repo.clone();
                match blocking(move || maintain::forget(&engine.db, &b.repo)).await {
                    Ok(()) => {
                        server.touch();
                        ok(j!({"repo": repo}))
                    }
                    Err(e) => error(StatusCode::CONFLICT, format!("{e:#}")),
                }
            }
            Err(refused) => *refused,
        },
        (&Method::POST, ["unplace"]) => match body::<UnplaceBody>(call) {
            Ok(b) => unplace(server, b).await,
            Err(refused) => *refused,
        },
        (&Method::POST, ["archive"]) => match body::<MoveBody>(call) {
            Ok(b) => move_model(server, Kind::Archive, b).await,
            Err(refused) => *refused,
        },
        (&Method::POST, ["restore"]) => match body::<MoveBody>(call) {
            Ok(b) => move_model(server, Kind::Restore, b).await,
            Err(refused) => *refused,
        },
        (&Method::GET, ["move"]) => ok(server.moving.borrow().clone()),
        (&Method::GET, ["gc"]) => {
            let offload = call
                .param("offload")
                .is_some_and(|v| v == "true" || v == "1");
            answer(
                blocking(move || {
                    let plan = maintain::gc_plan(&engine.db, &engine.store, offload)?;
                    Ok(gc_answer(&plan, None))
                })
                .await,
            )
        }
        (&Method::POST, ["gc"]) => match body::<GcBody>(call) {
            Ok(agreed) => {
                let done = blocking(move || {
                    // Only what is still removable now and was agreed to.
                    let mut plan =
                        maintain::gc_plan(&engine.db, &engine.store, !agreed.offload.is_empty())?;
                    plan.orphans.retain(|b| agreed.orphans.contains(&b.sha256));
                    plan.offload.retain(|b| agreed.offload.contains(&b.sha256));
                    let freed = maintain::gc_apply(&engine.db, &engine.store, &plan)?;
                    Ok(gc_answer(&plan, Some(freed)))
                })
                .await;
                server.touch();
                answer(done)
            }
            Err(refused) => *refused,
        },
        (&Method::GET, ["verify"]) => ok(server.check.lock().expect("lock").clone()),
        (&Method::POST, ["verify"]) => {
            tokio::spawn(daemon::check_store(server.clone()));
            ok(j!({"started": true}))
        }

        (&Method::GET, ["config"]) => answer(config_answer(server)),
        (&Method::PUT, ["config"]) => {
            let changes: serde_json::Map<String, Value> = match body(call) {
                Ok(b) => b,
                Err(refused) => return *refused,
            };
            match Config::update(&server.home, &changes) {
                Ok(config) => {
                    engine
                        .governor
                        .configure(config.max_connections(), config.speed_limit());
                    answer(config_answer(server))
                }
                Err(e) => error(StatusCode::BAD_REQUEST, format!("{e:#}")),
            }
        }
        (&Method::GET, ["log"]) => {
            let lines = call
                .param("lines")
                .and_then(|v| v.parse().ok())
                .unwrap_or(200usize)
                .min(5000);
            answer(log_tail(server, lines))
        }
        (&Method::POST, ["restart"]) => {
            let server = server.clone();
            tokio::spawn(async move {
                // Let this answer leave first.
                tokio::time::sleep(Duration::from_millis(200)).await;
                server.ask_restart();
            });
            ok(j!({"restarting": true}))
        }
        _ => plain(StatusCode::NOT_FOUND, "no such control endpoint"),
    }
}
