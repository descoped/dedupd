//! The progress of a running job: its rate, and what the fetcher reports.

use super::run::wait_text;
use super::{Job, Queue};
use crate::fetch::Wait;
use crate::now;
use crate::pull::PullWatch;
use dedupd_api::jobs::{FileStage, JobState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A rate measured from a running count.
pub(super) struct Meter {
    count: AtomicU64,
    sample: Mutex<(Instant, u64, u64)>,
}

impl Meter {
    pub(super) fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            sample: Mutex::new((Instant::now(), 0, 0)),
        }
    }

    pub(super) fn add(&self, bytes: u64) {
        self.count.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Bytes per second since the last look that was at least a second ago.
    pub(super) fn rate(&self) -> u64 {
        let mut sample = self.sample.lock().expect("lock");
        let elapsed = sample.0.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let count = self.count.load(Ordering::Relaxed);
            let rate = (count - sample.1) as f64 / elapsed.as_secs_f64();
            *sample = (Instant::now(), count, rate as u64);
        }
        sample.2
    }
}

/// Carries what the fetcher reports into the job's record.
pub(super) struct JobWatch {
    pub(super) queue: Arc<Queue>,
    pub(super) job: Arc<Job>,
    /// The job's `alive`, held for as long as any download reports here.
    pub(super) _alive: Arc<()>,
}

impl PullWatch for JobWatch {
    fn fetching(&self, _files: usize) {
        self.job.record().fetches = true;
        self.queue.changed();
    }

    fn bytes(&self, index: usize, delta: i64) {
        let resumed = {
            let mut r = self.job.record();
            let add = |value: u64| {
                if delta >= 0 {
                    value.saturating_add(delta as u64)
                } else {
                    value.saturating_sub(delta.unsigned_abs())
                }
            };
            r.bytes_done = add(r.bytes_done);
            if let Some(file) = r.files.get_mut(index) {
                file.done = add(file.done);
                if file.stage == FileStage::Waiting {
                    file.stage = FileStage::Downloading;
                }
            }
            // Bytes are arriving, so whatever the job was waiting for is over.
            delta > 0 && matches!(r.state, JobState::WaitingServer | JobState::WaitingNetwork)
        };
        if resumed {
            self.queue.set(&self.job, JobState::Downloading, None);
        } else {
            self.queue.ticked();
        }
    }

    fn received(&self, bytes: u64) {
        self.job.meter.add(bytes);
        self.queue.meter.add(bytes);
    }

    fn waiting(&self, _index: usize, why: Wait) {
        match why {
            Wait::Server { seconds } => {
                self.job.record().wait_until = Some(now() + seconds as i64);
                self.queue
                    .set(&self.job, JobState::WaitingServer, Some(wait_text(seconds)));
            }
            Wait::Network => {
                self.queue.set(
                    &self.job,
                    JobState::WaitingNetwork,
                    Some("the download server cannot be reached".to_string()),
                );
            }
        }
    }

    fn verifying(&self, index: usize) {
        let all_here = {
            let mut r = self.job.record();
            if let Some(file) = r.files.get_mut(index) {
                file.stage = FileStage::Verifying;
            }
            r.files
                .iter()
                .all(|f| matches!(f.stage, FileStage::Verifying | FileStage::Stored))
        };
        if all_here {
            self.queue.set(&self.job, JobState::Verifying, None);
        } else {
            self.queue.ticked();
        }
    }

    fn stored(&self, index: usize) {
        if let Some(file) = self.job.record().files.get_mut(index) {
            file.stage = FileStage::Stored;
            file.done = file.size;
        }
        self.queue.ticked();
    }
}
