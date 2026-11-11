//! A thin client for the dedupd daemon's control API.
//!
//! This is what the macOS app links, through UniFFI, and what the command
//! line uses when the daemon is running. It holds no state and never touches
//! the store: the daemon owns everything.

use serde::Deserialize;
use std::time::Duration;

uniffi::setup_scaffolding!();

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

fn agent(timeout: Option<Duration>) -> ureq::Agent {
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

fn url(address: &str, path: &str) -> String {
    format!("http://{address}/dedupd/v1/{path}")
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
