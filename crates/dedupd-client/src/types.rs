//! What the daemon's control API sends, as records Swift can hold.
//!
//! Each record has the shape of the JSON it is read from, so one definition
//! serves both. The recorded answers under
//! `crates/dedupd-server/tests/fixtures/control` keep the two in step.

use serde::Deserialize;

/// What the provider says about a model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct ModelMeta {
    pub author: Option<String>,
    pub pipeline_tag: Option<String>,
    pub library_name: Option<String>,
    pub license: Option<String>,
    pub base_model: Option<String>,
    /// Number of parameters.
    pub parameters: Option<u64>,
    pub architecture: Option<String>,
    pub context_length: Option<u64>,
    /// The owner must accept terms on the provider's site before downloading.
    pub gated: bool,
    pub private: bool,
    /// `gguf`, `mlx`, `safetensors`, `onnx`.
    pub formats: Vec<String>,
    pub languages: Vec<String>,
    pub tags: Vec<String>,
    pub downloads: Option<u64>,
    pub likes: Option<u64>,
    /// ISO 8601, as the provider sends it.
    pub last_modified: Option<String>,
    pub created_at: Option<String>,
    pub used_storage: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct AppPresence {
    pub app: String,
    pub files: u64,
    pub bytes: u64,
    /// Bytes that share their disk blocks with the store.
    pub shared_bytes: u64,
}

/// One repository in the library.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct LibraryModel {
    pub provider: String,
    pub repo: String,
    pub commit: Option<String>,
    pub meta: ModelMeta,
    pub revisions: u64,
    /// Seconds since the epoch.
    pub fetched_at: Option<i64>,
    pub stored_files: u64,
    pub stored_bytes: u64,
    pub apps: Vec<AppPresence>,
    pub verified: u64,
    pub mismatch: u64,
    pub not_upstream: u64,
    pub unchecked: u64,
    /// `ok`, `gone` or `unknown`.
    pub upstream: String,
    pub checked_at: Option<i64>,
    pub ollama_tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct RevisionFile {
    pub path: String,
    pub size: u64,
    pub sha256: Option<String>,
    pub git_oid: String,
    pub lfs: bool,
    /// On the internal disk.
    pub stored: bool,
    /// On the archive disk; `false` also when it is not attached.
    pub archived: bool,
    pub apps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct Revision {
    pub commit: String,
    pub fetched_at: i64,
    pub refs: Vec<String>,
    pub bytes: u64,
    pub files: Vec<RevisionFile>,
}

/// A file in an app's folder.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct Placement {
    pub app: String,
    pub path: String,
    pub rel_path: String,
    pub size: u64,
    pub sha256: String,
    pub shared: bool,
    pub verify_state: String,
    /// `adopted` when the app downloaded it, `placed` when dedupd put it there.
    pub origin: String,
}

/// A way a repository can be downloaded and used.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct Variant {
    /// `gguf`, or `files` for a repository that is taken whole.
    pub kind: String,
    pub name: String,
    /// What to ask for to get it: `org/repo:Q4_K_M`, or `org/repo`.
    pub spec: String,
    pub files: u64,
    pub bytes: u64,
    pub stored_bytes: u64,
    /// The apps that can use it.
    pub usable_by: Vec<String>,
    /// The apps that have it.
    pub placed_in: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct ModelDetail {
    pub model: LibraryModel,
    pub revisions: Vec<Revision>,
    pub placements: Vec<Placement>,
    pub variants: Vec<Variant>,
    /// The provider's own description, as it sent it.
    pub info_json: Option<String>,
    pub readme: Option<String>,
}

/// A folder on the disk and what dedupd knows is in it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct Location {
    /// `store`, `archive`, or an app's short name.
    pub id: String,
    pub path: String,
    pub present: bool,
    pub files: u64,
    pub bytes: u64,
    pub shared_bytes: u64,
    pub models: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct Locations {
    pub volume_total: u64,
    pub volume_free: u64,
    pub store: Location,
    pub archive: Option<Location>,
    pub apps: Vec<Location>,
}

/// One model of a search answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct SearchHit {
    pub id: String,
    pub meta: ModelMeta,
    pub archived: bool,
    pub apps: Vec<String>,
    pub downloading: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct SearchPage {
    pub models: Vec<SearchHit>,
    /// Hand this back as `cursor` to get the next page.
    pub next: Option<String>,
}

/// What to search the provider for. Empty fields are not asked about.
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct SearchQuery {
    pub text: String,
    /// `gguf`, `mlx`, `safetensors`, or empty for any.
    pub format: String,
    /// The provider's task name, for example `text-generation`.
    pub task: String,
    pub license: String,
    pub author: String,
    /// `downloads`, `likes`, `modified`, `created` or `trending`.
    pub sort: String,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct HubFile {
    pub path: String,
    pub size: u64,
    pub sha256: Option<String>,
    pub lfs: bool,
    pub stored: bool,
}

/// Everything about one repository on the provider.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct HubModel {
    pub id: String,
    pub commit: Option<String>,
    /// The provider could not be asked; this is what is archived.
    pub from_archive: bool,
    pub meta: ModelMeta,
    pub files: Vec<HubFile>,
    pub variants: Vec<Variant>,
    /// The same model in other formats and quantizations.
    pub related: Vec<SearchHit>,
    pub readme: Option<String>,
    pub archived: bool,
    pub apps: Vec<AppPresence>,
    pub downloading: bool,
    pub info_json: String,
}

/// What the provider allows in a time window, as it last said.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct Budget {
    pub name: String,
    pub quota: u64,
    pub remaining: u64,
    /// Seconds until the window starts again.
    pub resets_in: u64,
}

/// How hard the daemon is asking the provider right now.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct Governor {
    /// Requests in flight.
    pub connections: u64,
    /// Requests allowed at once at the moment.
    pub window: u64,
    pub max_connections: u64,
    /// Seconds the provider asked every request to wait; 0 when none.
    pub hold_seconds: u64,
    /// Bytes per second; 0 is no limit.
    pub speed_limit: u64,
    pub budgets: Vec<Budget>,
}

/// One download in the queue.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct Job {
    pub id: i64,
    pub model: String,
    pub repo: String,
    pub revision: String,
    pub quant: Option<String>,
    pub include: Vec<String>,
    pub apps: Vec<String>,
    pub offline: bool,
    /// `queued`, `resolving`, `downloading`, `verifying`, `placing`, `done`,
    /// `paused`, `waiting_server`, `waiting_network`, `needs_attention` or
    /// `cancelled`.
    pub state: String,
    /// Why the job is waiting or needs attention.
    pub detail: Option<String>,
    /// When a wait the provider asked for ends, in seconds since the epoch.
    pub wait_until: Option<i64>,
    pub commit: Option<String>,
    pub bytes_total: u64,
    pub bytes_done: u64,
    /// Bytes per second.
    pub speed: u64,
    #[serde(default)]
    pub files_total: u64,
    #[serde(default)]
    pub files_done: u64,
    pub created_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct Downloads {
    pub governor: Governor,
    /// Bytes per second of all jobs together.
    pub speed: u64,
    pub jobs: Vec<Job>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct JobFile {
    pub path: String,
    pub size: u64,
    pub done: u64,
    /// `waiting`, `downloading`, `verifying` or `stored`.
    pub stage: String,
    pub sha256: Option<String>,
    pub git_oid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct JobEvent {
    pub at: i64,
    pub text: String,
}

/// A download with its files and its history.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct JobDetail {
    pub job: Job,
    pub files: Vec<JobFile>,
    pub events: Vec<JobEvent>,
}

#[derive(Deserialize)]
pub(crate) struct WireJobDetail {
    #[serde(flatten)]
    job: Job,
    files: Vec<JobFile>,
    events: Vec<JobEvent>,
}

impl From<WireJobDetail> for JobDetail {
    fn from(w: WireJobDetail) -> Self {
        let mut job = w.job;
        job.files_total = w.files.len() as u64;
        job.files_done = w.files.iter().filter(|f| f.stage == "stored").count() as u64;
        Self {
            job,
            files: w.files,
            events: w.events,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct Enqueued {
    pub id: i64,
    /// The same download was already in the queue.
    pub existing: bool,
}

/// One line of the daemon's event stream: everything that changes often.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct DaemonEvent {
    pub seq: u64,
    /// Goes up when what the store or the apps hold has changed.
    pub library: u64,
    /// The whole store is being checked.
    pub checking: bool,
    pub downloads: Downloads,
}

/// The daemon as a process.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct DaemonInfo {
    pub pid: u32,
    pub started_at: i64,
    pub listen: String,
    pub executable: Option<String>,
    /// The launchd label it runs under; `None` when it was started by hand.
    pub managed_by: Option<String>,
    /// A Hugging Face token is sent with requests.
    pub token: bool,
    /// Where the token comes from: a variable's name or a file's path.
    pub token_source: Option<String>,
    pub max_connections: u64,
    pub speed_limit: u64,
    pub checking: bool,
    pub last_check_at: Option<i64>,
}

/// The settings in the daemon's config file, defaults filled in.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct DaemonSettings {
    pub path: String,
    pub listen: String,
    pub hf_endpoint: String,
    pub archive: Option<String>,
    pub bionic_models: String,
    pub ollama_models: String,
    pub hf_cache: String,
    pub reclaim: bool,
    pub set_env: bool,
    pub verify_days: u64,
    pub scan_interval: u64,
    pub max_connections: u64,
    pub speed_limit: u64,
    /// The running daemon still uses older settings.
    pub restart_needed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum SettingValue {
    Text {
        value: String,
    },
    Number {
        value: u64,
    },
    Flag {
        value: bool,
    },
    /// Remove the setting, so its default applies.
    Unset,
}

/// A setting to change, by its name in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingChange {
    pub key: String,
    pub value: SettingValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct PlannedFile {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct KeptFile {
    pub path: String,
    pub why: String,
}

/// What removing a repository from one app would do, or did.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct UnplacePlan {
    pub repo: String,
    pub app: String,
    pub remove: Vec<PlannedFile>,
    /// Links and manifests that go with the files.
    pub links: Vec<String>,
    pub keep: Vec<KeptFile>,
    pub bytes: u64,
    /// Files removed; `None` when this is only the plan.
    pub removed: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct StoredFile {
    pub sha256: String,
    pub size: u64,
    pub git_oid: Option<String>,
}

/// Stored files that could be removed, or were.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct GcPlan {
    /// Files nothing refers to.
    pub orphans: Vec<StoredFile>,
    /// Files that are on the archive disk and in no app.
    pub offload: Vec<StoredFile>,
    pub orphan_bytes: u64,
    pub offload_bytes: u64,
    /// Bytes freed; `None` when this is only the plan.
    pub freed: Option<u64>,
}

/// The check of the whole store.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct StoreCheck {
    pub running: bool,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub checked: u64,
    pub bytes: u64,
    pub damaged: Vec<String>,
    pub missing: Vec<String>,
    pub only_in_archive: u64,
    pub error: Option<String>,
}

/// What the provider said when it was asked about a repository.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct UpstreamCheck {
    /// `ok` or `gone`.
    pub upstream: String,
    pub commit: Option<String>,
    pub latest: Option<String>,
    /// `main` has moved since the model was archived.
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, uniffi::Record)]
pub struct LogTail {
    pub path: String,
    pub lines: Vec<String>,
}
