//! The control API under `/dedupd/v1`, for the command line and the app.

use crate::{Call, Server, daemon, json, plain};
use axum::http::{Method, StatusCode, header};
use axum::response::Response;
use dedupd_core::place::App;
use dedupd_core::pull::{self, PullRequest};
use dedupd_core::select::Selection;
use dedupd_core::{archive, status};
use serde::Deserialize;
use serde_json::json as j;
use std::sync::Arc;
use std::sync::atomic::Ordering;

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
    jobs: Option<usize>,
}

fn error(status: StatusCode, e: impl std::fmt::Display) -> Response {
    json(status, &j!({"error": e.to_string()}))
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
    match (&call.method, rest) {
        (&Method::GET, ["status"]) => {
            let s = match status::status(&server.engine.db()) {
                Ok(s) => s,
                Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
            };
            let archive_home = server.config.archive_home();
            json(
                StatusCode::OK,
                &j!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "home": server.home.root(),
                    "endpoint": server.engine.fetcher.endpoint(),
                    "archive": archive_home.as_ref().map(|a| j!({
                        "path": a.root(),
                        "attached": archive::attached(a),
                    })),
                    "downloading": {
                        "total": server.progress.total.load(Ordering::Relaxed),
                        "done": server.progress.done.load(Ordering::Relaxed),
                    },
                    "status": s,
                }),
            )
        }
        (&Method::POST, ["pull"]) => {
            let body: PullBody = match serde_json::from_slice(&call.body) {
                Ok(b) => b,
                Err(e) => return error(StatusCode::BAD_REQUEST, e),
            };
            let (repo, quant, revision) = match pull::parse_model(&body.model) {
                Ok(v) => v,
                Err(e) => return error(StatusCode::BAD_REQUEST, e),
            };
            let req = PullRequest {
                repo,
                revision,
                selection: Selection {
                    quant,
                    include: body.include,
                },
                apps: body.apps,
                offline: body.offline,
                jobs: body.jobs.unwrap_or(4),
            };
            match pull::pull(&server.engine, &req, &server.progress).await {
                Ok(out) => {
                    // The daemon keeps the archive disk current after its
                    // own pulls, as the command line does after its.
                    let server = server.clone();
                    tokio::spawn(async move {
                        let _ = daemon::pass(&server).await;
                    });
                    json(
                        StatusCode::OK,
                        &serde_json::to_value(out).unwrap_or_default(),
                    )
                }
                Err(e) => error(StatusCode::BAD_GATEWAY, format!("{e:#}")),
            }
        }
        (&Method::POST, ["scan"]) => match daemon::pass(server).await {
            Ok(report) => json(
                StatusCode::OK,
                &serde_json::to_value(report).unwrap_or_default(),
            ),
            Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
        },
        _ => plain(StatusCode::NOT_FOUND, "no such control endpoint"),
    }
}
