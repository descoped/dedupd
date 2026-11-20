//! The control API under `/dedupd/v1`, for the command line and the app.
//!
//! Everything the app shows comes from here, and everything it does goes
//! through here: the daemon owns the store, the database and the downloads.

use crate::answer::{Refused, answer, blocking, body, error, failure, internal, ok};
use crate::{Call, Server, daemon, favorites, moves, plain};
use axum::body::{Body, Bytes};
use axum::http::{Method, StatusCode, header};
use axum::response::Response;
use dedupd_api::activity::ActivityList;
use dedupd_api::app::AppList;
use dedupd_api::daemon::{
    DaemonEvent, DaemonInfo, DaemonSettings, DaemonStatus, LogTail, Setting, Started,
};
use dedupd_api::favorites::FavoriteBody;
use dedupd_api::jobs::{Cleared, Enqueued, JobAction, JobRef, JobState, PullBody};
use dedupd_api::library::{ModelList, RepoBody};
use dedupd_api::moves::{GcBody, MoveBody, MoveKind, UnplaceBody};
use dedupd_api::url::decode_path;
use dedupd_core::config::Config;
use dedupd_core::db::NotInLibrary;
use dedupd_core::hub::HubClient;
use dedupd_core::profile::{self, Installed};
use dedupd_core::{Home, activity, archive, library, maintain, status};
use serde_json::Value;
use std::collections::BTreeMap;
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

/// The launchd label this process runs under; `None` when it was started
/// by hand.
fn launchd_label() -> Option<String> {
    let name = std::env::var("XPC_SERVICE_NAME").ok()?;
    let by_launchd = std::os::unix::process::parent_id() == 1;
    (by_launchd && name != "0" && !name.starts_with("application.")).then_some(name)
}

fn path_text(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

fn status_answer(server: &Server) -> anyhow::Result<DaemonStatus> {
    let s = status::status(&server.engine.db(), &server.engine.store)?;
    let archive_home = server.config.archive_home();
    let (total, done) = server.queue.totals();
    let governor = server.engine.governor.state();
    Ok(DaemonStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        home: path_text(server.home.root()),
        endpoint: server.engine.fetcher.endpoint().to_string(),
        archive_path: archive_home.as_ref().map(|a| path_text(a.root())),
        archive_attached: archive_home.as_ref().is_some_and(Home::attached),
        store_files: s.store_files,
        store_bytes: s.store_bytes,
        shared_bytes: s.shared_bytes,
        downloading_total: total,
        downloading_done: done,
        apps: s.apps,
        repositories: s.repos,
        daemon: DaemonInfo {
            pid: std::process::id(),
            started_at: server.started_at,
            listen: server.config.listen().to_string(),
            executable: std::env::current_exe().ok().map(|p| path_text(&p)),
            managed_by: launchd_label(),
            token: server.engine.hub.has_token(),
            token_source: HubClient::token_source(),
            max_connections: governor.max_connections,
            speed_limit: governor.speed_limit,
            checking: server.check.lock().expect("lock").running,
            last_check_at: daemon::last_check(server),
        },
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
        moving: tokio::sync::watch::Receiver<Option<dedupd_api::moves::ModelMove>>,
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
        let event = DaemonEvent {
            seq: feed.seq,
            library,
            checking: feed.server.check.lock().expect("lock").running,
            moving,
            downloads: feed.server.queue.downloads(),
        };
        let mut line = serde_json::to_string(&event).unwrap_or_default();
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
/// Refused while the model is being archived or restored: the job would
/// fetch or place the files the move is taking away or bringing back.
fn enqueue(server: &Arc<Server>, body: PullBody) -> Result<(i64, bool), Refused> {
    let _starting = server.starting.lock().expect("lock");
    if let Some(why) = moves::refuses_job(server.moving.borrow().as_ref(), &body.model) {
        return Err(Box::new(error(StatusCode::CONFLICT, why)));
    }
    server
        .queue
        .add_or_find(
            &body.model,
            body.include,
            body.apps,
            body.offline,
            body.kind,
        )
        .map_err(|e| Box::new(error(StatusCode::BAD_REQUEST, e)))
}

async fn download_action(server: &Arc<Server>, id: &str, action: &str) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return error(StatusCode::BAD_REQUEST, "a download is named by its number");
    };
    let Some(action) = JobAction::parse(action) else {
        return plain(StatusCode::NOT_FOUND, "no such control endpoint");
    };
    match server.queue.act(id, action).await {
        Ok(()) => ok(JobRef { id }),
        Err(e) => error(StatusCode::CONFLICT, e),
    }
}

/// Ask the provider whether it still has the repository and where `main`
/// is now. Nothing is downloaded.
async fn check_upstream(server: &Arc<Server>, repo: &str) -> Response {
    match library::check_upstream(&server.engine, repo).await {
        Ok(found) => {
            server.touch();
            ok(found)
        }
        Err(e) => failure(e),
    }
}

async fn unplace(server: &Arc<Server>, body: UnplaceBody) -> Response {
    let engine = server.engine.clone();
    let told = server.clone();
    let done = blocking(move || {
        let done = maintain::unplace(
            &engine.db,
            &engine.store,
            &engine.dirs,
            &body.repo,
            body.app,
            body.paths.as_deref(),
            body.apply,
        );
        // Also after a failure part of the way: the app's files and the
        // activity list may have changed. Said here, where the work is
        // done, so it is said also when the request was dropped meanwhile.
        if done.is_ok() || body.apply {
            told.touch();
        }
        done
    })
    .await;
    answer(done)
}

/// Archive or restore one model: say what it would do, or start doing it.
async fn move_model(server: &Arc<Server>, kind: MoveKind, body: MoveBody) -> Response {
    if body.apply {
        return match moves::start(server, kind, body.repo).await {
            Ok(()) => ok(Started { started: true }),
            Err(why) => error(StatusCode::CONFLICT, why),
        };
    }
    let server = server.clone();
    let planned = blocking(move || {
        let e = &server.engine;
        Ok(match kind {
            MoveKind::Archive => {
                ok(archive::archive_plan(&e.db, &e.store, &e.dirs, &body.repo)?.answer())
            }
            MoveKind::Restore => ok(archive::restore_plan(&e.db, &e.store, &body.repo)?.answer()),
        })
    })
    .await;
    planned.unwrap_or_else(|e| error(StatusCode::CONFLICT, format!("{e:#}")))
}

/// The settings as they are in the config file, with the defaults filled
/// in, and whether the running daemon still uses older ones.
fn config_answer(server: &Server) -> anyhow::Result<DaemonSettings> {
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
    Ok(DaemonSettings {
        path: path_text(&Config::path(&server.home)),
        listen: file.listen().to_string(),
        hf_endpoint: file.hf_endpoint().to_string(),
        archive: file.archive.as_deref().map(path_text),
        bionic_models: path_text(file.bionic_models.as_ref().unwrap_or(&dirs.bionic)),
        ollama_models: path_text(file.ollama_models.as_ref().unwrap_or(&dirs.ollama)),
        hf_cache: path_text(file.hf_cache.as_ref().unwrap_or(&dirs.hf_cache)),
        reclaim: file.reclaim(),
        set_env: file.set_env(),
        verify_days: file.verify_days(),
        scan_interval: file.scan_interval(),
        max_connections: file.max_connections() as u64,
        speed_limit: file.speed_limit(),
        restart_needed: fixed(&file) != fixed(&server.config),
    })
}

/// The last `lines` lines of the daemon's log.
fn log_tail(server: &Server, lines: usize) -> anyhow::Result<LogTail> {
    use std::io::{Read, Seek, SeekFrom};
    let path = server.home.log_path();
    let mut text = Vec::new();
    if let Ok(mut file) = std::fs::File::open(&path) {
        let size = file.metadata()?.len();
        file.seek(SeekFrom::Start(size.saturating_sub(LOG_TAIL)))?;
        file.read_to_end(&mut text)?;
    }
    let text = String::from_utf8_lossy(&text);
    let all: Vec<&str> = text.lines().collect();
    let from = all.len().saturating_sub(lines);
    Ok(LogTail {
        path: path_text(&path),
        lines: all[from..].iter().map(|l| l.to_string()).collect(),
    })
}

pub(crate) async fn handle(server: &Arc<Server>, call: &Call, rest: &[&str]) -> Response {
    let engine = server.engine.clone();
    match (&call.method, rest) {
        (&Method::GET, ["status"]) => {
            favorites::refresh_token(server);
            answer(status_answer(server))
        }
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
            // A restart does not wait for this call: the job is in the
            // queue and goes on after it.
            let settled = tokio::select! {
                settled = server.queue.settled(id) => settled,
                () = server.shutdown.cancelled() => return restarting(
                    "the download is in the queue and continues after the restart",
                ),
            };
            match settled {
                Ok(job) if job.state == JobState::Done => ok(job.outcome),
                Ok(job) => error(
                    StatusCode::BAD_GATEWAY,
                    job.detail
                        .unwrap_or_else(|| format!("the download is {}", job.state.as_str())),
                ),
                Err(e) => internal(format!("{e:#}")),
            }
        }
        (&Method::POST, ["scan"]) => {
            // On a task of its own: a caller that goes away does not cut the
            // pass off part of the way, and a restart does not wait for it.
            let pass = {
                let server = server.clone();
                tokio::spawn(async move { daemon::pass(&server).await })
            };
            tokio::select! {
                done = pass => match done {
                    Ok(done) => answer(done),
                    Err(e) => internal(format!("the scan stopped: {e}")),
                },
                () = server.shutdown.cancelled() => restarting("the scan was not finished"),
            }
        }

        (&Method::GET, ["events"]) => events(server),
        (&Method::GET, ["downloads"]) => ok(server.queue.downloads()),
        (&Method::POST, ["downloads"]) => {
            let body: PullBody = match body(call) {
                Ok(b) => b,
                Err(refused) => return *refused,
            };
            match enqueue(server, body) {
                Ok((id, existing)) => ok(Enqueued { id, existing }),
                Err(refused) => *refused,
            }
        }
        (&Method::POST, ["downloads", "clear"]) => match server.queue.clear_finished() {
            Ok(removed) => ok(Cleared {
                removed: removed as u64,
            }),
            Err(e) => internal(format!("{e:#}")),
        },
        (&Method::GET, ["downloads", id]) => match id.parse().map(|id| server.queue.get(id)) {
            Ok(Ok(job)) => ok(job.detail()),
            _ => error(StatusCode::NOT_FOUND, format!("there is no download {id}")),
        },
        (&Method::POST, ["downloads", id, action]) => download_action(server, id, action).await,

        (&Method::GET, ["apps"]) => answer(
            blocking(move || {
                Ok(AppList {
                    apps: profile::apps(&Installed::detect(&engine.dirs)),
                })
            })
            .await,
        ),
        (&Method::GET, ["models"]) => answer(
            blocking(move || {
                Ok(ModelList {
                    models: library::models(&engine.db())?,
                })
            })
            .await,
        ),
        (&Method::GET, ["models", name @ ..]) if !name.is_empty() => {
            let repo = decode_path(&name.join("/"));
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
                Ok(None) => error(StatusCode::NOT_FOUND, NotInLibrary(repo).to_string()),
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
                let told = server.clone();
                let forgotten = blocking(move || {
                    maintain::forget(&engine.db, &b.repo)?;
                    // Said where the work is done: a dropped request does
                    // not keep the library's change from the app.
                    told.touch();
                    Ok(())
                });
                match forgotten.await {
                    Ok(()) => ok(RepoBody { repo }),
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
            Ok(b) => move_model(server, MoveKind::Archive, b).await,
            Err(refused) => *refused,
        },
        (&Method::POST, ["restore"]) => match body::<MoveBody>(call) {
            Ok(b) => move_model(server, MoveKind::Restore, b).await,
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
                    Ok(plan.answer(None))
                })
                .await,
            )
        }
        (&Method::POST, ["gc"]) => match body::<GcBody>(call) {
            Ok(agreed) => {
                let told = server.clone();
                let done = blocking(move || {
                    // Only what is still removable now and was agreed to.
                    let mut plan =
                        maintain::gc_plan(&engine.db, &engine.store, !agreed.offload.is_empty())?;
                    plan.orphans.retain(|b| agreed.orphans.contains(&b.sha256));
                    plan.offload.retain(|b| agreed.offload.contains(&b.sha256));
                    let freed = maintain::gc_apply(&engine.db, &engine.store, &plan);
                    // Also after a failure part of the way, and also when
                    // the request was dropped meanwhile.
                    told.touch();
                    Ok(plan.answer(Some(freed?)))
                })
                .await;
                answer(done)
            }
            Err(refused) => *refused,
        },
        (&Method::GET, ["verify"]) => ok(server.check.lock().expect("lock").clone()),
        (&Method::POST, ["verify"]) => {
            tokio::spawn(daemon::check_store(server.clone()));
            ok(Started { started: true })
        }

        (&Method::GET, ["config"]) => answer(config_answer(server)),
        (&Method::PUT, ["config"]) => {
            let changes: BTreeMap<Setting, Value> = match body(call) {
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
        (&Method::GET, ["activity"]) => {
            let limit = call
                .param("limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(200usize)
                .min(1000);
            let home = engine.store.home().clone();
            answer(
                blocking(move || {
                    Ok(ActivityList {
                        entries: activity::recent(&home, limit),
                    })
                })
                .await,
            )
        }
        (&Method::GET, ["favorites"]) => {
            favorites::refresh_token(server);
            ok(favorites::current(server))
        }
        (&Method::POST, ["favorites"]) => match body::<FavoriteBody>(call) {
            Ok(b) if !dedupd_core::names::is_repo_name(&b.repo) => error(
                StatusCode::BAD_REQUEST,
                format!("{:?} is not a model's name", b.repo),
            ),
            Ok(b) => match favorites::set(server, &b.repo, b.favorite) {
                Ok(list) => ok(list),
                Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
            },
            Err(refused) => *refused,
        },
        (&Method::POST, ["favorites", "sync"]) => match favorites::sync(server, true).await {
            Ok(list) => ok(list),
            Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
        },
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
            ok(Started { started: true })
        }
        _ => plain(StatusCode::NOT_FOUND, "no such control endpoint"),
    }
}

/// The answer to a call that was waiting when the daemon was told to
/// restart: the restart does not wait for it.
fn restarting(what: &str) -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        format!("dedupd is restarting; {what}"),
    )
}
