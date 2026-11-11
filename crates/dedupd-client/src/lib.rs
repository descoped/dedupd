//! A thin client for the dedupd daemon's control API.
//!
//! This is what the macOS app links, through UniFFI, and what the command
//! line uses when the daemon is running. It holds no state and never touches
//! the store: the daemon owns everything.

mod events;
mod types;

pub use events::{EventListener, EventSubscription, subscribe};
pub use types::*;

use serde::Deserialize;
use std::time::Duration;

uniffi::setup_scaffolding!();

/// How long a call that only reads or changes records may take.
const QUICK: Duration = Duration::from_secs(20);
/// How long a call that asks the provider may take.
const PATIENT: Duration = Duration::from_secs(90);

/// Where the daemon listens unless `listen` in its `config.toml` says
/// otherwise. The same value as the core library's default.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:8642";

/// The address to try when the user has not chosen one.
#[uniffi::export]
pub fn default_address() -> String {
    DEFAULT_ADDRESS.to_string()
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum ClientError {
    /// Nothing answered at the address: the daemon is not running.
    #[error("the dedupd daemon is not running at {address}")]
    NotRunning { address: String },
    /// The daemon answered that the request failed.
    #[error("{message}")]
    Failed { message: String },
    /// The answer was not what this client understands.
    #[error("unexpected answer from the daemon: {message}")]
    Protocol { message: String },
}

#[derive(Debug, Clone, Deserialize, uniffi::Record)]
pub struct AppUsage {
    pub app: String,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Deserialize, uniffi::Record)]
pub struct Repository {
    pub provider: String,
    pub name: String,
    pub commit: Option<String>,
    /// Files of the archived revisions that are in the store.
    #[serde(default)]
    pub stored_files: u64,
    #[serde(default)]
    pub stored_bytes: u64,
    /// App files that hold this repository's content.
    pub files: u64,
    pub bytes: u64,
    pub verified: u64,
    pub mismatch: u64,
    pub not_upstream: u64,
    pub unchecked: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DaemonStatus {
    pub version: String,
    pub home: String,
    pub endpoint: String,
    /// The archive folder, when one is configured.
    pub archive_path: Option<String>,
    pub archive_attached: bool,
    pub store_files: u64,
    pub store_bytes: u64,
    /// Bytes of app files that share disk blocks with the store.
    pub shared_bytes: u64,
    /// Bytes of the downloads in flight, and how many have arrived.
    pub downloading_total: u64,
    pub downloading_done: u64,
    pub apps: Vec<AppUsage>,
    pub repositories: Vec<Repository>,
    /// The daemon as a process, and its limits.
    pub daemon: DaemonInfo,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct PlacedApp {
    pub app: String,
    pub cloned: u64,
    pub present: u64,
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct PullResult {
    pub repo: String,
    pub commit: String,
    pub from_archive: bool,
    pub files: u64,
    pub bytes: u64,
    pub downloaded: u64,
    pub placed: Vec<PlacedApp>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, uniffi::Record)]
pub struct ScanResult {
    pub forgotten: u64,
    pub adopted: u64,
    pub adopted_bytes: u64,
    pub reclaimed: u64,
    pub reclaimed_bytes: u64,
    pub archived: u64,
    pub errors: Vec<String>,
}

// The wire shapes, as the daemon's control API sends them, where they differ
// from the records above.

#[derive(Deserialize)]
struct WireArchive {
    path: String,
    attached: bool,
}

#[derive(Deserialize)]
struct WireProgress {
    total: u64,
    done: u64,
}

#[derive(Deserialize)]
struct WireStore {
    store_files: u64,
    store_bytes: u64,
    shared_bytes: u64,
    apps: Vec<AppUsage>,
    repos: Vec<Repository>,
}

#[derive(Deserialize)]
struct WireStatus {
    version: String,
    home: String,
    endpoint: String,
    archive: Option<WireArchive>,
    downloading: WireProgress,
    daemon: DaemonInfo,
    status: WireStore,
}

#[derive(Deserialize)]
struct WirePlaced {
    cloned: u64,
    present: u64,
    conflicts: Vec<String>,
}

#[derive(Deserialize)]
struct WirePull {
    repo: String,
    commit: String,
    from_archive: bool,
    files: u64,
    bytes: u64,
    downloaded: u64,
    placed: Vec<(String, WirePlaced)>,
    notes: Vec<String>,
}

impl From<WirePull> for PullResult {
    fn from(w: WirePull) -> Self {
        Self {
            repo: w.repo,
            commit: w.commit,
            from_archive: w.from_archive,
            files: w.files,
            bytes: w.bytes,
            downloaded: w.downloaded,
            placed: w
                .placed
                .into_iter()
                .map(|(app, p)| PlacedApp {
                    app,
                    cloned: p.cloned,
                    present: p.present,
                    conflicts: p.conflicts,
                })
                .collect(),
            notes: w.notes,
        }
    }
}

impl PullResult {
    /// Read the outcome of a pull as the daemon sends it, which is also how
    /// the core library's own outcome serialises.
    pub fn from_json(text: &str) -> Result<Self, ClientError> {
        parse::<WirePull>(text).map(Self::from)
    }
}

#[derive(Deserialize)]
struct WireError {
    error: String,
}

pub(crate) fn agent(timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_global(timeout)
        .http_status_as_error(false)
        .build()
        .into()
}

fn finish<T: serde::de::DeserializeOwned>(
    address: &str,
    sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<T, ClientError> {
    let mut resp = sent.map_err(|e| match e {
        ureq::Error::Timeout(_) => ClientError::Failed {
            message: "the daemon did not answer in time".into(),
        },
        _ => ClientError::NotRunning {
            address: address.to_string(),
        },
    })?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| ClientError::Protocol {
            message: e.to_string(),
        })?;
    if !(200..300).contains(&status) {
        let message = serde_json::from_str::<WireError>(&text)
            .map(|e| e.error)
            .unwrap_or(text);
        return Err(ClientError::Failed { message });
    }
    parse(&text)
}

fn parse<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, ClientError> {
    serde_json::from_str(text).map_err(|e| ClientError::Protocol {
        message: e.to_string(),
    })
}

pub(crate) fn url(address: &str, path: &str) -> String {
    format!("http://{address}/dedupd/v1/{path}")
}

/// Percent-encode a value for a query string.
fn encode(text: &str) -> String {
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

/// A repository name as part of a path: encoded, with its `/` kept.
fn encode_repo(repo: &str) -> String {
    repo.split('/').map(encode).collect::<Vec<_>>().join("/")
}

fn get<T: serde::de::DeserializeOwned>(
    address: &str,
    path: &str,
    timeout: Duration,
) -> Result<T, ClientError> {
    finish(address, agent(Some(timeout)).get(url(address, path)).call())
}

fn post<T: serde::de::DeserializeOwned>(
    address: &str,
    path: &str,
    body: &serde_json::Value,
    timeout: Duration,
) -> Result<T, ClientError> {
    let sent = agent(Some(timeout))
        .post(url(address, path))
        .send_json(body);
    finish(address, sent)
}

/// What the daemon holds and what it is doing. Fails with `NotRunning` when
/// no daemon answers, which is also how to find out whether one is there.
#[uniffi::export]
pub fn status(address: String) -> Result<DaemonStatus, ClientError> {
    let sent = agent(Some(Duration::from_secs(10)))
        .get(url(&address, "status"))
        .call();
    let w: WireStatus = finish(&address, sent)?;
    Ok(DaemonStatus {
        version: w.version,
        home: w.home,
        endpoint: w.endpoint,
        archive_attached: w.archive.as_ref().is_some_and(|a| a.attached),
        archive_path: w.archive.map(|a| a.path),
        store_files: w.status.store_files,
        store_bytes: w.status.store_bytes,
        shared_bytes: w.status.shared_bytes,
        downloading_total: w.downloading.total,
        downloading_done: w.downloading.done,
        apps: w.status.apps,
        repositories: w.status.repos,
        daemon: w.daemon,
    })
}

/// Fetch a model into the store and place it into `apps` (`bionic`,
/// `ollama`, `hf`). Returns when the daemon has finished, which for a large
/// model takes as long as the download; `status` reports progress meanwhile.
/// With `offline`, only what is archived is used.
#[uniffi::export]
pub fn pull(
    address: String,
    model: String,
    apps: Vec<String>,
    include: Vec<String>,
    offline: bool,
) -> Result<PullResult, ClientError> {
    let body = serde_json::json!({
        "model": model, "apps": apps, "include": include, "offline": offline,
    });
    let sent = agent(None).post(url(&address, "pull")).send_json(&body);
    finish::<WirePull>(&address, sent).map(PullResult::from)
}

/// Have the daemon look at the app folders now instead of waiting for its
/// next pass.
#[uniffi::export]
pub fn scan(address: String) -> Result<ScanResult, ClientError> {
    let sent = agent(None).post(url(&address, "scan")).send_empty();
    finish(&address, sent)
}

/// Every repository the daemon knows.
#[uniffi::export]
pub fn models(address: String) -> Result<Vec<LibraryModel>, ClientError> {
    #[derive(Deserialize)]
    struct Wire {
        models: Vec<LibraryModel>,
    }
    get::<Wire>(&address, "models", QUICK).map(|w| w.models)
}

/// Everything about one repository in the library: its revisions and
/// files, where each app has them, and how it can be used.
#[uniffi::export]
pub fn model(address: String, repo: String) -> Result<ModelDetail, ClientError> {
    get(&address, &format!("models/{}", encode_repo(&repo)), QUICK)
}

/// The store, the archive and each app's folder, with what is in them.
#[uniffi::export]
pub fn locations(address: String) -> Result<Locations, ClientError> {
    get(&address, "locations", QUICK)
}

/// Search the provider. The daemon asks, with its token and within the
/// provider's limits, and marks what is already here.
#[uniffi::export]
pub fn search(address: String, query: SearchQuery) -> Result<SearchPage, ClientError> {
    let mut parts = Vec::new();
    for (name, value) in [
        ("q", query.text.as_str()),
        ("format", query.format.as_str()),
        ("task", query.task.as_str()),
        ("license", query.license.as_str()),
        ("author", query.author.as_str()),
        ("sort", query.sort.as_str()),
        ("cursor", query.cursor.as_deref().unwrap_or("")),
    ] {
        if !value.is_empty() {
            parts.push(format!("{name}={}", encode(value)));
        }
    }
    get(
        &address,
        &format!("hub/search?{}", parts.join("&")),
        PATIENT,
    )
}

/// Everything the provider says about one repository, before anything is
/// downloaded: its files, the ways it can be used, and related models.
#[uniffi::export]
pub fn hub_model(address: String, repo: String) -> Result<HubModel, ClientError> {
    get(
        &address,
        &format!("hub/models/{}", encode_repo(&repo)),
        PATIENT,
    )
}

/// The download queue. The event stream delivers the same thing whenever
/// it changes; this is for asking once.
#[uniffi::export]
pub fn downloads(address: String) -> Result<Downloads, ClientError> {
    get(&address, "downloads", QUICK)
}

/// One download with its files and its history.
#[uniffi::export]
pub fn download(address: String, id: i64) -> Result<JobDetail, ClientError> {
    get::<WireJobDetail>(&address, &format!("downloads/{id}"), QUICK).map(JobDetail::from)
}

/// Queue a download of `model` (`org/repo`, `org/repo:Q4_K_M` or
/// `org/repo@revision`) to be placed into `apps`. Returns at once; the
/// event stream tells how it goes. With `offline`, only what is archived is
/// used, which places an archived model without asking the provider.
#[uniffi::export]
pub fn enqueue(
    address: String,
    model: String,
    apps: Vec<String>,
    include: Vec<String>,
    offline: bool,
) -> Result<Enqueued, ClientError> {
    let body = serde_json::json!({
        "model": model, "apps": apps, "include": include, "offline": offline,
    });
    post(&address, "downloads", &body, QUICK)
}

/// Act on a download: `pause`, `resume`, `retry`, `start` (now, ahead of
/// the queue), `cancel`, or `remove` (a finished one from the list).
#[uniffi::export]
pub fn download_action(address: String, id: i64, action: String) -> Result<(), ClientError> {
    let path = format!("downloads/{id}/{}", encode(&action));
    post::<serde_json::Value>(&address, &path, &serde_json::json!({}), QUICK).map(|_| ())
}

/// Drop the finished and cancelled downloads from the list. Returns how
/// many there were.
#[uniffi::export]
pub fn clear_downloads(address: String) -> Result<u64, ClientError> {
    #[derive(Deserialize)]
    struct Wire {
        removed: u64,
    }
    post::<Wire>(&address, "downloads/clear", &serde_json::json!({}), QUICK).map(|w| w.removed)
}

/// Ask the provider whether it still has `repo` and whether `main` moved.
#[uniffi::export]
pub fn check_upstream(address: String, repo: String) -> Result<UpstreamCheck, ClientError> {
    post(
        &address,
        "check",
        &serde_json::json!({"repo": repo}),
        PATIENT,
    )
}

/// Drop `repo` from the archive records, so its files can be cleaned up.
/// Refused while an app still has files of it.
#[uniffi::export]
pub fn forget(address: String, repo: String) -> Result<(), ClientError> {
    let body = serde_json::json!({"repo": repo});
    post::<serde_json::Value>(&address, "forget", &body, QUICK).map(|_| ())
}

/// What removing `repo` from `app` would delete; with `apply`, delete it.
/// `paths` limits the removal to the files the owner saw and agreed to.
/// The store keeps everything, so the model can be placed again.
#[uniffi::export]
pub fn unplace(
    address: String,
    repo: String,
    app: String,
    apply: bool,
    paths: Option<Vec<String>>,
) -> Result<UnplacePlan, ClientError> {
    let body = serde_json::json!({"repo": repo, "app": app, "apply": apply, "paths": paths});
    post(&address, "unplace", &body, PATIENT)
}

/// The stored files that could be removed. With `offload`, also the local
/// copies of files that are on the archive disk and in no app.
#[uniffi::export]
pub fn gc_plan(address: String, offload: bool) -> Result<GcPlan, ClientError> {
    get(&address, &format!("gc?offload={offload}"), PATIENT)
}

/// Remove exactly the files named, by sha256, as far as they are still
/// removable. `orphans` are deleted for good; `offload` loses only its
/// local copy.
#[uniffi::export]
pub fn gc_apply(
    address: String,
    orphans: Vec<String>,
    offload: Vec<String>,
) -> Result<GcPlan, ClientError> {
    let body = serde_json::json!({"orphans": orphans, "offload": offload});
    post(&address, "gc", &body, PATIENT)
}

/// Start a check of the whole store. It runs in the daemon, for as long
/// as hashing everything takes.
#[uniffi::export]
pub fn start_store_check(address: String) -> Result<(), ClientError> {
    post::<serde_json::Value>(&address, "verify", &serde_json::json!({}), QUICK).map(|_| ())
}

/// The running check, or what the last one found.
#[uniffi::export]
pub fn store_check(address: String) -> Result<StoreCheck, ClientError> {
    get(&address, "verify", QUICK)
}

/// The daemon's settings.
#[uniffi::export]
pub fn settings(address: String) -> Result<DaemonSettings, ClientError> {
    get(&address, "config", QUICK)
}

/// Change settings in the daemon's config file. The download limits apply
/// at once; the answer says whether the rest needs a restart.
#[uniffi::export]
pub fn update_settings(
    address: String,
    changes: Vec<SettingChange>,
) -> Result<DaemonSettings, ClientError> {
    let mut body = serde_json::Map::new();
    for change in changes {
        let value = match change.value {
            SettingValue::Text { value } => serde_json::Value::String(value),
            SettingValue::Number { value } => serde_json::json!(value),
            SettingValue::Flag { value } => serde_json::Value::Bool(value),
            SettingValue::Unset => serde_json::Value::Null,
        };
        body.insert(change.key, value);
    }
    let sent = agent(Some(QUICK))
        .put(url(&address, "config"))
        .send_json(serde_json::Value::Object(body));
    finish(&address, sent)
}

/// The last `lines` lines of the daemon's log.
#[uniffi::export]
pub fn log_tail(address: String, lines: u32) -> Result<LogTail, ClientError> {
    get(&address, &format!("log?lines={lines}"), QUICK)
}

/// Have the daemon stop and start again, which makes it read its settings.
#[uniffi::export]
pub fn restart(address: String) -> Result<(), ClientError> {
    post::<serde_json::Value>(&address, "restart", &serde_json::json!({}), QUICK).map(|_| ())
}
