//! The local HTTP front ends: the Hugging Face Hub API and the Ollama
//! registry API on one port, answered from the store and the archive, with
//! everything else passed through to Hugging Face.

mod control;
pub mod daemon;
mod files;
mod hubapi;
mod proxy;
mod registry;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::Response;
use dedupd_core::Home;
use dedupd_core::config::Config;
use dedupd_core::fetch::Progress;
use dedupd_core::pull::Engine;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct Server {
    pub engine: Arc<Engine>,
    pub config: Config,
    pub home: Home,
    /// Branch and tag names resolved recently, so a burst of requests for one
    /// model asks upstream once.
    commits: Mutex<HashMap<(String, String), (String, Instant)>>,
    /// Large files being put together from what clients download through us.
    assemblies: Mutex<HashMap<String, Arc<files::Assembly>>>,
    pub progress: Arc<Progress>,
    /// Pulls running through the control API; see `begin_pull`.
    pulls: Mutex<usize>,
    /// What the background pass remembers between rounds.
    pub(crate) passes: daemon::PassState,
}

/// One pull running through the control API. `progress` starts again from
/// zero when the last one ends, so a failed pull leaves nothing behind.
pub(crate) struct PullGuard<'a>(&'a Server);

impl Drop for PullGuard<'_> {
    fn drop(&mut self) {
        let mut pulls = self.0.pulls.lock().expect("lock");
        *pulls -= 1;
        if *pulls == 0 {
            self.0.progress.total.store(0, Ordering::Relaxed);
            self.0.progress.done.store(0, Ordering::Relaxed);
        }
    }
}

impl Server {
    pub fn new(engine: Arc<Engine>, config: Config, home: Home) -> Arc<Self> {
        // Partly assembled files from an earlier run cannot be continued:
        // which ranges they hold was only known in memory.
        if let Ok(dir) = std::fs::read_dir(home.tmp_dir()) {
            for entry in dir.flatten() {
                if entry.file_name().to_string_lossy().ends_with(".asm") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Arc::new(Self {
            engine,
            config,
            home,
            commits: Mutex::new(HashMap::new()),
            assemblies: Mutex::new(HashMap::new()),
            progress: Arc::new(Progress::default()),
            pulls: Mutex::new(0),
            passes: daemon::PassState::default(),
        })
    }

    /// Count a pull as running until the returned guard is dropped.
    pub(crate) fn begin_pull(&self) -> PullGuard<'_> {
        *self.pulls.lock().expect("lock") += 1;
        PullGuard(self)
    }

    /// Forget which commits branch names were last seen at, so the next
    /// request asks upstream again.
    pub fn forget_refs(&self) {
        self.commits.lock().expect("lock").clear();
    }
}

pub fn router(server: Arc<Server>) -> Router {
    Router::new().fallback(handle).with_state(server)
}

/// Serve until the listener fails or `shutdown` resolves.
pub async fn serve(
    server: Arc<Server>,
    listener: tokio::net::TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    axum::serve(listener, router(server))
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

/// One request as the handlers see it.
#[derive(Clone)]
pub(crate) struct Call {
    pub method: Method,
    pub path: String,
    pub query: Option<String>,
    pub headers: HeaderMap,
    pub body: axum::body::Bytes,
    /// `http://host:port` as the client addressed us; links we hand out point here.
    pub origin: String,
}

impl Call {
    pub fn is_head(&self) -> bool {
        self.method == Method::HEAD
    }

    pub fn header(&self, name: header::HeaderName) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn path_and_query(&self) -> String {
        match &self.query {
            Some(q) => format!("{}?{q}", self.path),
            None => self.path.clone(),
        }
    }

    /// The values given for `name` in the query string, undecoded.
    pub fn query_params<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        let pairs = self.query.as_deref().unwrap_or("").split('&');
        pairs.filter_map(move |p| p.strip_prefix(name)?.strip_prefix('='))
    }

    /// `body`, or nothing when the client only asked for the headers.
    pub fn body_unless_head(&self, body: impl Into<Body>) -> Body {
        if self.is_head() {
            Body::empty()
        } else {
            body.into()
        }
    }
}

pub(crate) fn plain(status: StatusCode, text: impl Into<String>) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(text.into()))
        .expect("valid response")
}

pub(crate) fn json(status: StatusCode, value: &serde_json::Value) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(value.to_string()))
        .expect("valid response")
}

async fn handle(State(server): State<Arc<Server>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, 16 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return plain(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
    };
    let host = parts
        .headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(server.config.listen());
    let call = Call {
        method: parts.method,
        path: parts.uri.path().to_string(),
        query: parts.uri.query().map(str::to_string),
        origin: format!("http://{host}"),
        headers: parts.headers,
        body,
    };

    let segments: Vec<&str> = call.path.split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        ["dedupd", "v1", rest @ ..] => control::handle(&server, &call, rest).await,
        ["dedupd", "blob", digest] => registry::blob_direct(&server, &call, digest).await,
        ["v2"] => json(StatusCode::OK, &serde_json::json!({})),
        ["v2", rest @ ..] => registry::handle(&server, &call, rest).await,
        ["api", "models", rest @ ..] => hubapi::api(&server, &call, rest).await,
        ["datasets" | "spaces" | "api", ..] => proxy::pass_through(&server, &call).await,
        [a, "resolve", rev, file @ ..] if !file.is_empty() => {
            hubapi::resolve(&server, &call, a, rev, &file.join("/")).await
        }
        [a, b, "resolve", rev, file @ ..] if !file.is_empty() => {
            hubapi::resolve(&server, &call, &format!("{a}/{b}"), rev, &file.join("/")).await
        }
        _ => proxy::pass_through(&server, &call).await,
    }
}
