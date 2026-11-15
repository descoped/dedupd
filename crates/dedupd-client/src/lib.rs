//! A thin client for the dedupd daemon's control API.
//!
//! This is what the macOS app links, through UniFFI, and what the command
//! line uses when the daemon is running. It holds no state and never touches
//! the store: the daemon owns everything.

mod events;

pub use events::{EventListener, EventSubscription, subscribe};

use dedupd_api::activity::{ActivityEntry, ActivityList};
use dedupd_api::app::App;
use dedupd_api::daemon::{
    DEFAULT_ADDRESS, DaemonSettings, DaemonStatus, Failure, LogTail, ScanResult, SettingChange,
    StoreCheck,
};
use dedupd_api::hub::{HubModel, SearchPage, SearchQuery};
use dedupd_api::jobs::{
    Cleared, Downloads, Enqueued, JobAction, JobDetail, JobKind, PullBody, PullResult,
};
use dedupd_api::library::{
    LibraryModel, Locations, ModelDetail, ModelList, RepoBody, UpstreamCheck,
};
use dedupd_api::moves::{
    ArchivePlan, GcBody, GcPlan, ModelMove, MoveBody, RestorePlan, UnplaceBody, UnplacePlan,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::time::Duration;

uniffi::setup_scaffolding!();

/// How long a call that only reads or changes records may take.
const QUICK: Duration = Duration::from_secs(20);
/// How long a call that asks the provider may take.
const PATIENT: Duration = Duration::from_secs(90);

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

pub(crate) fn agent(timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_global(timeout)
        .http_status_as_error(false)
        .build()
        .into()
}

fn finish<T: DeserializeOwned>(
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
        let message = serde_json::from_str::<Failure>(&text)
            .map(|e| e.error)
            .unwrap_or(text);
        return Err(ClientError::Failed { message });
    }
    parse(&text)
}

fn parse<T: DeserializeOwned>(text: &str) -> Result<T, ClientError> {
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

fn get<T: DeserializeOwned>(
    address: &str,
    path: &str,
    timeout: Duration,
) -> Result<T, ClientError> {
    finish(address, agent(Some(timeout)).get(url(address, path)).call())
}

fn post<T: DeserializeOwned>(
    address: &str,
    path: &str,
    body: &impl Serialize,
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
    get(&address, "status", Duration::from_secs(10))
}

fn pull_body(
    model: String,
    apps: Vec<App>,
    include: Vec<String>,
    offline: bool,
    kind: JobKind,
) -> PullBody {
    PullBody {
        model,
        apps,
        include,
        offline,
        kind,
    }
}

/// Fetch a model into the store and activate it in `apps`. Returns when the
/// daemon has finished, which for a large model takes as long as the
/// download; `status` reports progress meanwhile. With `offline`, only what
/// dedupd has kept is used. `kind` says whether the job is shown as a
/// download or as an activation of a stored model.
#[uniffi::export]
pub fn pull(
    address: String,
    model: String,
    apps: Vec<App>,
    include: Vec<String>,
    offline: bool,
    kind: JobKind,
) -> Result<PullResult, ClientError> {
    let body = pull_body(model, apps, include, offline, kind);
    let sent = agent(None).post(url(&address, "pull")).send_json(&body);
    finish(&address, sent)
}

/// Have the daemon look at the app folders now instead of waiting for its
/// next pass.
#[uniffi::export]
pub fn scan(address: String) -> Result<ScanResult, ClientError> {
    let sent = agent(None).post(url(&address, "scan")).send_empty();
    finish(&address, sent)
}

/// Every model the daemon knows.
#[uniffi::export]
pub fn models(address: String) -> Result<Vec<LibraryModel>, ClientError> {
    get::<ModelList>(&address, "models", QUICK).map(|list| list.models)
}

/// Everything about one model in the library: its revisions and files,
/// where each app has them, and how it can be used.
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

/// Everything the provider says about one model, before anything is
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

/// What happened to the models in the apps, newest first: activations,
/// deactivations, archives and restores, each with its result.
#[uniffi::export]
pub fn activity(address: String) -> Result<Vec<ActivityEntry>, ClientError> {
    get::<ActivityList>(&address, "activity", QUICK).map(|list| list.entries)
}

/// One job with its files and its history.
#[uniffi::export]
pub fn download(address: String, id: i64) -> Result<JobDetail, ClientError> {
    get(&address, &format!("downloads/{id}"), QUICK)
}

/// Queue `model` (`org/repo`, `org/repo:Q4_K_M` or `org/repo@revision`) to
/// be activated in `apps`. Returns at once; the event stream tells how it
/// goes. With `offline`, only what dedupd has kept is used, which activates
/// a stored model without asking the provider. `kind` says whether the job
/// is shown as a download or as an activation.
#[uniffi::export]
pub fn enqueue(
    address: String,
    model: String,
    apps: Vec<App>,
    include: Vec<String>,
    offline: bool,
    kind: JobKind,
) -> Result<Enqueued, ClientError> {
    let body = pull_body(model, apps, include, offline, kind);
    post(&address, "downloads", &body, QUICK)
}

/// Act on a job. The job's `actions` say which ones its state allows; the
/// daemon refuses the others.
#[uniffi::export]
pub fn download_action(address: String, id: i64, action: JobAction) -> Result<(), ClientError> {
    let path = format!("downloads/{id}/{}", action.as_str());
    post::<serde_json::Value>(&address, &path, &serde_json::json!({}), QUICK).map(|_| ())
}

/// Drop the finished and cancelled jobs from the list. Returns how many
/// there were.
#[uniffi::export]
pub fn clear_downloads(address: String) -> Result<u64, ClientError> {
    post::<Cleared>(&address, "downloads/clear", &serde_json::json!({}), QUICK).map(|c| c.removed)
}

/// Ask the provider whether it still has `repo` and whether `main` moved.
#[uniffi::export]
pub fn check_upstream(address: String, repo: String) -> Result<UpstreamCheck, ClientError> {
    post(&address, "check", &RepoBody { repo }, PATIENT)
}

/// Drop `repo` from the library, so its files can be cleaned up.
/// Refused while an app still has files of it.
#[uniffi::export]
pub fn forget(address: String, repo: String) -> Result<(), ClientError> {
    post::<serde_json::Value>(&address, "forget", &RepoBody { repo }, QUICK).map(|_| ())
}

/// What taking `repo` out of `app` would delete; with `apply`, delete it.
/// `paths` limits the removal to the files the owner saw and agreed to.
/// The store keeps everything, so the model can be activated again.
#[uniffi::export]
pub fn unplace(
    address: String,
    repo: String,
    app: App,
    apply: bool,
    paths: Option<Vec<String>>,
) -> Result<UnplacePlan, ClientError> {
    let body = UnplaceBody {
        repo,
        app,
        apply,
        paths,
    };
    post(&address, "unplace", &body, PATIENT)
}

/// What archiving `repo` would do: which apps it is taken out of, and how
/// much leaves the internal disk. Nothing is changed. Refused when the
/// archive disk is not attached.
#[uniffi::export]
pub fn archive_plan(address: String, repo: String) -> Result<ArchivePlan, ClientError> {
    let body = MoveBody { repo, apply: false };
    post(&address, "archive", &body, PATIENT)
}

/// Archive `repo`: out of every app and off the internal disk, once the
/// archive disk holds a good copy of every file. Returns at once; the event
/// stream tells how it goes (`moving`).
#[uniffi::export]
pub fn archive_model(address: String, repo: String) -> Result<(), ClientError> {
    let body = MoveBody { repo, apply: true };
    post::<serde_json::Value>(&address, "archive", &body, PATIENT).map(|_| ())
}

/// What restoring `repo` would copy back from the archive disk.
#[uniffi::export]
pub fn restore_plan(address: String, repo: String) -> Result<RestorePlan, ClientError> {
    let body = MoveBody { repo, apply: false };
    post(&address, "restore", &body, PATIENT)
}

/// Copy `repo` back from the archive disk. Returns at once; the event
/// stream tells how it goes (`moving`). The model is put into no app.
#[uniffi::export]
pub fn restore_model(address: String, repo: String) -> Result<(), ClientError> {
    let body = MoveBody { repo, apply: true };
    post::<serde_json::Value>(&address, "restore", &body, PATIENT).map(|_| ())
}

/// The archive or restore that is running, or the last one; `None` when
/// there has been none since the daemon started.
#[uniffi::export]
pub fn model_move(address: String) -> Result<Option<ModelMove>, ClientError> {
    get(&address, "move", QUICK)
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
    post(&address, "gc", &GcBody { orphans, offload }, PATIENT)
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
    let body: BTreeMap<_, _> = changes.into_iter().map(|c| (c.key, c.value)).collect();
    let sent = agent(Some(QUICK))
        .put(url(&address, "config"))
        .send_json(&body);
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
