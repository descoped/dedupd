//! The download queue: jobs, their files, what can be done to them, and
//! what a finished one did.

use crate::app::App;
use serde::{Deserialize, Serialize};

/// Where a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum JobState {
    Queued,
    Resolving,
    Downloading,
    Verifying,
    Placing,
    Done,
    /// Stopped by the owner; continues when they say so.
    Paused,
    /// The server asked for a pause; continues by itself.
    WaitingServer,
    /// The server cannot be reached; continues by itself.
    WaitingNetwork,
    /// Stopped for a reason only the owner can do something about.
    NeedsAttention,
    Cancelled,
}

impl JobState {
    pub const fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Resolving => "resolving",
            JobState::Downloading => "downloading",
            JobState::Verifying => "verifying",
            JobState::Placing => "placing",
            JobState::Done => "done",
            JobState::Paused => "paused",
            JobState::WaitingServer => "waiting_server",
            JobState::WaitingNetwork => "waiting_network",
            JobState::NeedsAttention => "needs_attention",
            JobState::Cancelled => "cancelled",
        }
    }

    /// A task is working on the job.
    pub const fn is_working(self) -> bool {
        matches!(
            self,
            JobState::Resolving
                | JobState::Downloading
                | JobState::Verifying
                | JobState::Placing
                | JobState::WaitingServer
                | JobState::WaitingNetwork
        )
    }

    /// The daemon is working on the job, or will by itself.
    pub const fn is_active(self) -> bool {
        matches!(self, JobState::Queued) || self.is_working()
    }

    /// Nothing more will happen to the job.
    pub const fn is_finished(self) -> bool {
        matches!(self, JobState::Done | JobState::Cancelled)
    }

    /// What the owner can do to a job in this state, in the order the app
    /// shows them. The queue refuses anything else.
    pub const fn actions(self) -> &'static [JobAction] {
        use JobAction::*;
        match self {
            JobState::Queued => &[Start, Pause, Cancel],
            JobState::Resolving
            | JobState::Downloading
            | JobState::Verifying
            | JobState::WaitingServer
            | JobState::WaitingNetwork => &[Pause, Cancel],
            // Putting files into apps is not cut off part of the way.
            JobState::Placing => &[],
            JobState::Paused => &[Resume, Cancel],
            JobState::NeedsAttention => &[Retry, Cancel],
            JobState::Done | JobState::Cancelled => &[Remove],
        }
    }

    pub fn allows(self, action: JobAction) -> bool {
        self.actions().contains(&action)
    }
}

/// What the owner can do to a job in this state; the rule of
/// [`JobState::actions`] for callers across the FFI, such as the app's
/// canned daemon.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn job_actions(state: JobState) -> Vec<JobAction> {
    state.actions().to_vec()
}

/// [`JobState::is_active`] for callers across the FFI.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn job_is_active(state: JobState) -> bool {
    state.is_active()
}

/// [`JobState::is_finished`] for callers across the FFI.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn job_is_finished(state: JobState) -> bool {
    state.is_finished()
}

/// What can be done to a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum JobAction {
    /// Stop for now; what is downloaded is kept.
    Pause,
    /// Continue a paused job.
    Resume,
    /// Try again a job that needed attention.
    Retry,
    /// Start a queued job now, ahead of the others.
    Start,
    /// Stop and give up; what was downloaded for it alone is removed.
    Cancel,
    /// Take a finished job off the list.
    Remove,
}

impl JobAction {
    pub const ALL: [JobAction; 6] = [
        JobAction::Pause,
        JobAction::Resume,
        JobAction::Retry,
        JobAction::Start,
        JobAction::Cancel,
        JobAction::Remove,
    ];

    /// The word in the control API's path: `downloads/{id}/{action}`.
    pub const fn as_str(self) -> &'static str {
        match self {
            JobAction::Pause => "pause",
            JobAction::Resume => "resume",
            JobAction::Retry => "retry",
            JobAction::Start => "start",
            JobAction::Cancel => "cancel",
            JobAction::Remove => "remove",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.as_str() == word)
    }
}

/// What a job is for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum JobKind {
    /// Fetch a model, and put it into the apps named.
    #[default]
    Download,
    /// Put a model that is stored into the apps named; anything missing is
    /// fetched on the way.
    Activation,
}

/// Where one file of a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum FileStage {
    Waiting,
    Downloading,
    Verifying,
    Stored,
}

/// One file of a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct JobFile {
    pub path: String,
    pub size: u64,
    /// Bytes on disk.
    pub done: u64,
    pub stage: FileStage,
    /// sha256 of the content, for files the provider keeps in LFS.
    pub sha256: Option<String>,
    pub git_oid: String,
}

/// One line of a job's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct JobEvent {
    /// Seconds since the Unix epoch.
    pub at: i64,
    pub text: String,
}

/// An app that did not take a model, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct NotActivated {
    pub app: App,
    /// Why, in a sentence that does not name the app in front.
    pub reason: String,
    /// Trying again may work (the provider out of reach, a full disk);
    /// otherwise the same answer would come again.
    #[serde(default)]
    pub temporary: bool,
}

impl NotActivated {
    /// An answer that would come again.
    pub fn lasting(app: App, reason: impl Into<String>) -> Self {
        Self {
            app,
            reason: reason.into(),
            temporary: false,
        }
    }
}

/// One job in the queue, without its files and history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Job {
    pub id: i64,
    /// The model as it was asked for: `org/repo[:quant][@revision]`.
    pub model: String,
    pub repo: String,
    pub revision: String,
    pub quant: Option<String>,
    pub include: Vec<String>,
    pub apps: Vec<App>,
    pub offline: bool,
    /// A download, or an activation of a stored model. A job that downloads
    /// files is a download, also when it was asked as an activation.
    pub kind: JobKind,
    pub state: JobState,
    /// Why the job is waiting or needs attention.
    pub detail: Option<String>,
    /// When a wait the provider asked for ends, in seconds since the epoch.
    pub wait_until: Option<i64>,
    pub commit: Option<String>,
    pub bytes_total: u64,
    pub bytes_done: u64,
    /// Bytes per second.
    pub speed: u64,
    pub files_total: u64,
    pub files_done: u64,
    /// Once the job is done: the apps asked for that took the model.
    pub activated: Vec<App>,
    /// Once the job is done: the apps asked for that did not, each with why.
    pub not_activated: Vec<NotActivated>,
    /// Once the job is done: anything else worth knowing (the kept copy was
    /// used, files were left alone).
    pub notes: Vec<String>,
    /// What the owner can do to it now.
    pub actions: Vec<JobAction>,
    pub created_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
}

/// One job with its files and its history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct JobDetail {
    pub job: Job,
    pub files: Vec<JobFile>,
    pub events: Vec<JobEvent>,
}

/// What the provider allows in a time window, as it last said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Budget {
    pub name: String,
    pub quota: u64,
    pub remaining: u64,
    /// Seconds until the window starts again.
    pub resets_in: u64,
}

/// How hard the daemon is asking the provider right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
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

/// The queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Downloads {
    pub governor: Governor,
    /// Bytes per second of all jobs together.
    pub speed: u64,
    pub jobs: Vec<Job>,
}

/// The answer to a request to queue a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Enqueued {
    pub id: i64,
    /// The same job was already in the queue.
    pub existing: bool,
}

/// What a request to queue a job asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullBody {
    /// `org/repo`, `org/repo:Q4_K_M` or `org/repo@revision`.
    pub model: String,
    #[serde(default)]
    pub apps: Vec<App>,
    #[serde(default)]
    pub include: Vec<String>,
    /// Use only what dedupd has kept; never ask the provider.
    #[serde(default)]
    pub offline: bool,
    #[serde(default)]
    pub kind: JobKind,
}

/// A job, named in an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRef {
    pub id: i64,
}

/// The jobs taken off the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cleared {
    pub removed: u64,
}

/// What putting a model's files into one app did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PlacedApp {
    pub app: App,
    /// Files created as clones.
    pub cloned: u64,
    /// Files that were already there with the right content.
    pub present: u64,
    /// Files that exist with other content and were left alone.
    pub conflicts: Vec<String>,
}

/// What a pull did: the files it fetched and where it put them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PullResult {
    pub repo: String,
    pub commit: String,
    /// The provider could not be asked or no longer has the repository, and
    /// the copy dedupd kept was used.
    pub from_archive: bool,
    pub files: u64,
    pub bytes: u64,
    /// Files that had to be downloaded, over every run of the job.
    pub downloaded: u64,
    /// Each app the files were put into. One with conflicts did not take
    /// the model as a whole, and is in `skipped` too.
    pub placed: Vec<PlacedApp>,
    /// The apps asked for that did not take the model, each with why.
    pub skipped: Vec<NotActivated>,
    /// Things that did not stop the pull but the owner should know.
    pub notes: Vec<String>,
}

impl PullResult {
    /// The apps that took the model: placed, with nothing left in the way.
    pub fn activated(&self) -> Vec<App> {
        self.placed
            .iter()
            .filter(|p| p.conflicts.is_empty() && !self.skipped.iter().any(|s| s.app == p.app))
            .map(|p| p.app)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_reads_back_from_its_word() {
        for action in JobAction::ALL {
            assert_eq!(JobAction::parse(action.as_str()), Some(action));
        }
        assert_eq!(JobAction::parse("explode"), None);
    }

    #[test]
    fn a_state_allows_what_the_app_offers() {
        assert!(JobState::NeedsAttention.allows(JobAction::Retry));
        assert!(!JobState::Done.allows(JobAction::Cancel));
        assert!(JobState::Downloading.allows(JobAction::Pause));
        assert!(!JobState::Downloading.allows(JobAction::Remove));
        for state in [JobState::Done, JobState::Cancelled] {
            assert!(state.is_finished());
            assert_eq!(state.actions(), [JobAction::Remove]);
        }
    }

    #[test]
    fn an_app_with_conflicts_or_a_skip_did_not_take_the_model() {
        let placed = |app, conflicts: &[&str]| PlacedApp {
            app,
            cloned: 1,
            present: 0,
            conflicts: conflicts.iter().map(|c| c.to_string()).collect(),
        };
        let result = PullResult {
            repo: "o/r".into(),
            commit: "c".into(),
            from_archive: false,
            files: 1,
            bytes: 1,
            downloaded: 0,
            placed: vec![
                placed(App::Bionic, &[]),
                placed(App::Ollama, &["x"]),
                placed(App::Hf, &[]),
            ],
            skipped: vec![NotActivated::lasting(App::Hf, "no")],
            notes: vec![],
        };
        assert_eq!(result.activated(), [App::Bionic]);
    }
}
