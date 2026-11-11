//! The local HTTP front ends: the Hugging Face Hub API and the Ollama
//! registry API on one port, answered from the store and the archive, with
//! everything else passed through to Hugging Face.

mod control;
pub mod daemon;
mod discover;
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
use dedupd_core::pull::Engine;
use dedupd_core::queue::Queue;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub struct Server {
    pub engine: Arc<Engine>,
    pub config: Config,
    pub home: Home,
    /// Branch and tag names resolved recently, so a burst of requests for one
    /// model asks upstream once.
    commits: Mutex<HashMap<(String, String), (String, Instant)>>,
    /// Large files being put together from what clients download through us.
    assemblies: Mutex<HashMap<String, Arc<files::Assembly>>>,
    /// The downloads the daemon is doing or has been asked to do.
    pub queue: Arc<Queue>,
    /// What the background pass remembers between rounds.
    pub(crate) passes: daemon::PassState,
    /// The store check the owner asked for, or the scheduled one.
    pub(crate) check: Mutex<daemon::CheckState>,
    /// Counts up whenever what the store or the apps hold has changed.
    library: watch::Sender<u64>,
    /// Answers from the provider's search, kept for a few minutes.
    pub(crate) searches: Mutex<HashMap<String, (Instant, serde_json::Value)>>,
    /// Seconds since the epoch when this daemon started.
    pub started_at: i64,
    /// Cancelled when the daemon is stopping; ends the long-lived answers.
    pub shutdown: CancellationToken,
    /// Set when the daemon was asked to start again after stopping.
    restart: AtomicBool,
}

impl Server {
    /// Open the daemon's state. Downloads that were going when the daemon
    /// last stopped continue from here, so this needs a running runtime.
    pub fn new(engine: Arc<Engine>, config: Config, home: Home) -> anyhow::Result<Arc<Self>> {
        // Partly assembled files from an earlier run cannot be continued:
        // which ranges they hold was only known in memory.
        if let Ok(dir) = std::fs::read_dir(home.tmp_dir()) {
            for entry in dir.flatten() {
                if entry.file_name().to_string_lossy().ends_with(".asm") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        let queue = Queue::open(engine.clone())?;
        let server = Arc::new(Self {
            engine,
            config,
            home,
            commits: Mutex::new(HashMap::new()),
            assemblies: Mutex::new(HashMap::new()),
            queue,
            passes: daemon::PassState::default(),
            check: Mutex::new(daemon::CheckState::default()),
            library: watch::Sender::new(0),
            searches: Mutex::new(HashMap::new()),
            started_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            shutdown: CancellationToken::new(),
            restart: AtomicBool::new(false),
        });
        // The daemon keeps the archive disk current after its own
        // downloads, as the command line does after its.
        let weak = Arc::downgrade(&server);
        server.queue.on_done(move || {
            if let Some(server) = weak.upgrade() {
                tokio::spawn(async move {
                    let _ = daemon::pass(&server).await;
                });
            }
        });
        server.queue.start();
        Ok(server)
    }

    /// Say that what the store or the apps hold has changed, so whoever is
    /// watching looks again.
    pub fn touch(&self) {
        self.library.send_modify(|v| *v += 1);
    }

    pub(crate) fn library_changes(&self) -> watch::Receiver<u64> {
        self.library.subscribe()
    }

    /// Stop serving, and have the process start again afterwards.
    pub fn ask_restart(&self) {
        self.restart.store(true, Ordering::Relaxed);
        self.shutdown.cancel();
    }

    /// Whether `ask_restart` was called.
    pub fn restart_asked(&self) -> bool {
        self.restart.load(Ordering::Relaxed)
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
    let stopping = server.shutdown.clone();
    axum::serve(listener, router(server))
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = shutdown => {}
                _ = stopping.cancelled() => {}
            }
            // The event streams end here; without that the server would
            // wait for them for ever.
            stopping.cancel();
        })
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

    /// The first value given for `name` in the query string, decoded.
    pub fn param(&self, name: &str) -> Option<String> {
        self.query_params(name).next().map(percent_decode)
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

/// Undo the percent-encoding of a query value; `+` is a space.
pub(crate) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let hex = |i: usize| bytes.get(i).and_then(|b| (*b as char).to_digit(16));
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                i += 2;
            }
            (b'+', _, _) => out.push(b' '),
            (b, _, _) => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode a value for a query string.
pub(crate) fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
        ["dedupd", "v1", "hub", rest @ ..] => discover::handle(&server, &call, rest).await,
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
