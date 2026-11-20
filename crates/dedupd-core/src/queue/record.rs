//! What a job is, as the database keeps it and the owner sees it.

use super::MAX_EVENTS;
use crate::fetch::Expect;
use crate::hub::TreeEntry;
use crate::now;
use crate::pull::PullRequest;
use anyhow::Result;
use dedupd_api::app::App;
use dedupd_api::jobs::{
    FileStage, Job as JobSummary, JobDetail, JobEvent, JobFile, JobKind, JobState, NotActivated,
    PullResult,
};
use serde::{Deserialize, Serialize};

/// Where a file of a job comes from, as the fetcher knows it.
pub(super) fn expect_of(file: &JobFile) -> Expect {
    match &file.sha256 {
        Some(sha) => Expect::Sha256(sha.clone()),
        None => Expect::GitOid(file.git_oid.clone()),
    }
}

/// One job, as it is kept in the database and shown to the owner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: i64,
    /// The model as it was asked for: `org/repo[:quant][@revision]`.
    pub model: String,
    pub repo: String,
    pub revision: String,
    pub quant: Option<String>,
    pub include: Vec<String>,
    pub apps: Vec<App>,
    pub offline: bool,
    /// Older builds kept no kind; their jobs read as downloads.
    #[serde(default)]
    pub kind: JobKind,
    pub state: JobState,
    /// Why the job is waiting or needs attention, in words.
    pub detail: Option<String>,
    /// When a wait the server asked for ends, in seconds since the epoch.
    pub wait_until: Option<i64>,
    /// The commit the job settled on when it first resolved the revision.
    pub commit: Option<String>,
    pub bytes_total: u64,
    pub bytes_done: u64,
    /// Bytes per second over the last moments.
    #[serde(default)]
    pub speed: u64,
    pub files: Vec<JobFile>,
    pub events: Vec<JobEvent>,
    /// The job has files to download, as it found when it looked at the
    /// store; such a job is a download, whatever it was asked as.
    #[serde(default)]
    pub fetches: bool,
    /// What the pull did, once it is done.
    pub outcome: Option<PullResult>,
    pub created_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
}

impl JobRecord {
    pub(super) fn request(&self, jobs: usize) -> Result<PullRequest> {
        PullRequest::from_spec(
            &self.model,
            self.include.clone(),
            self.apps.clone(),
            self.offline,
            jobs,
        )
    }

    pub(super) fn note(&mut self, text: impl Into<String>) {
        self.events.push(JobEvent {
            at: now(),
            text: text.into(),
        });
        if self.events.len() > MAX_EVENTS {
            let extra = self.events.len() - MAX_EVENTS;
            self.events.drain(..extra);
        }
    }

    /// Where the job belongs: with the downloads, or with the activations.
    /// A job that downloads files is a download, also when it was asked as
    /// an activation (a model on a detached archive disk is fetched again);
    /// one asked as a download that found every file stored, and so only
    /// put the model into apps, is an activation.
    pub fn shown_as(&self) -> JobKind {
        let fetched = self.outcome.as_ref().map(|o| o.downloaded);
        if self.fetches || fetched.is_some_and(|n| n > 0) {
            JobKind::Download
        } else if self.kind == JobKind::Activation || (fetched == Some(0) && !self.apps.is_empty())
        {
            JobKind::Activation
        } else {
            JobKind::Download
        }
    }

    /// The apps the finished job put the model into.
    pub fn activated(&self) -> Vec<App> {
        self.outcome
            .as_ref()
            .map(PullResult::activated)
            .unwrap_or_default()
    }

    /// The apps the finished job was asked for that did not take the model,
    /// each with why. A job kept by an older build, which wrote no reasons,
    /// gets a plain one.
    pub fn not_activated(&self) -> Vec<NotActivated> {
        let Some(outcome) = self.outcome.as_ref() else {
            return Vec::new();
        };
        let activated = outcome.activated();
        let mut skipped = outcome.skipped.clone();
        for app in &self.apps {
            if !activated.contains(app) && !skipped.iter().any(|s| s.app == *app) {
                skipped.push(NotActivated {
                    app: *app,
                    reason: "no reason was kept".to_string(),
                    temporary: false,
                });
            }
        }
        skipped
    }

    /// What else the finished job has to say (the kept copy was used, files
    /// were left alone).
    pub fn notes(&self) -> Vec<String> {
        self.outcome
            .as_ref()
            .map(|o| o.notes.clone())
            .unwrap_or_default()
    }

    /// The job as the owner sees it in a list: without its files and
    /// history, with what it did and what can be done to it.
    pub fn summary(&self) -> JobSummary {
        let files_done = self
            .files
            .iter()
            .filter(|f| f.stage == FileStage::Stored)
            .count();
        JobSummary {
            id: self.id,
            model: self.model.clone(),
            repo: self.repo.clone(),
            revision: self.revision.clone(),
            quant: self.quant.clone(),
            include: self.include.clone(),
            apps: self.apps.clone(),
            offline: self.offline,
            kind: self.shown_as(),
            state: self.state,
            detail: self.detail.clone(),
            wait_until: self.wait_until,
            commit: self.commit.clone(),
            bytes_total: self.bytes_total,
            bytes_done: self.bytes_done,
            speed: self.speed,
            files_total: self.files.len() as u64,
            files_done: files_done as u64,
            activated: self.activated(),
            not_activated: self.not_activated(),
            notes: self.notes(),
            actions: self.state.actions().to_vec(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            finished_at: self.finished_at,
        }
    }

    /// The job with its files and its history.
    pub fn detail(&self) -> JobDetail {
        JobDetail {
            job: self.summary(),
            files: self.files.clone(),
            events: self.events.clone(),
        }
    }
}

pub(super) fn file_state(entry: &TreeEntry) -> JobFile {
    JobFile {
        path: entry.path.clone(),
        size: entry.size,
        done: 0,
        stage: FileStage::Waiting,
        sha256: entry.lfs_sha256.clone(),
        git_oid: entry.git_oid.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dedupd_api::jobs::JobAction;

    /// A finished job as the database keeps it, with the given outcome.
    fn finished(outcome: serde_json::Value) -> JobRecord {
        let mut whole = serde_json::json!({
            "repo": "org/model", "commit": "c0", "from_archive": false,
            "files": 3, "bytes": 3, "downloaded": 0, "placed": [], "skipped": [], "notes": [],
        });
        whole
            .as_object_mut()
            .unwrap()
            .extend(outcome.as_object().unwrap().clone());
        serde_json::from_value(serde_json::json!({
            "id": 1, "model": "org/model", "repo": "org/model", "revision": "main",
            "quant": null, "include": [], "apps": ["bionic", "ollama"], "offline": false,
            "state": "done", "detail": null, "wait_until": null, "commit": null,
            "bytes_total": 1, "bytes_done": 1, "files": [], "events": [],
            "outcome": whole, "created_at": 0, "updated_at": 0, "finished_at": 0,
        }))
        .unwrap()
    }

    fn placed(app: &str, conflicts: &[&str]) -> serde_json::Value {
        serde_json::json!({"app": app, "cloned": 3, "present": 0, "conflicts": conflicts})
    }

    #[test]
    fn a_finished_job_says_which_apps_took_the_model() {
        // Files of the app's own in the way: the app did not take the model.
        let blocked = finished(serde_json::json!({
            "placed": [placed("bionic", &[]), placed("ollama", &["blobs/sha256-ab"])],
        }));
        assert_eq!(blocked.activated(), [App::Bionic]);
        assert_eq!(blocked.not_activated()[0].app, App::Ollama);
        // It downloaded nothing, so it reads as an activation.
        assert_eq!(blocked.shown_as(), JobKind::Activation);
        let summary = blocked.summary();
        assert_eq!(summary.kind, JobKind::Activation);
        assert_eq!(summary.actions, [JobAction::Remove]);

        // An app the outcome does not mention gets a plain reason.
        let silent = finished(serde_json::json!({"placed": [placed("bionic", &[])]}));
        let not = silent.not_activated();
        assert_eq!((not.len(), not[0].app), (1, App::Ollama));
        assert_eq!(not[0].reason, "no reason was kept");

        // A job that has not finished has nothing to say yet.
        let mut working = blocked.clone();
        working.outcome = None;
        working.state = JobState::Downloading;
        assert!(working.activated().is_empty() && working.not_activated().is_empty());
        assert_eq!(
            working.summary().actions,
            [JobAction::Pause, JobAction::Cancel]
        );
    }

    #[test]
    fn a_job_that_downloads_files_is_a_download_whatever_it_was_asked_as() {
        let mut job = finished(serde_json::json!({"placed": [], "downloaded": 2}));
        job.kind = JobKind::Activation;
        assert_eq!(job.shown_as(), JobKind::Download);
        job.outcome = None;
        assert_eq!(job.shown_as(), JobKind::Activation);
        job.fetches = true;
        assert_eq!(job.shown_as(), JobKind::Download);
    }
}
