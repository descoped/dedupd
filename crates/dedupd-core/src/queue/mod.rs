//! The daemon's download queue.
//!
//! A job is a pull that can be watched, paused, continued and cancelled, and
//! that survives a restart: every job is a row in the database, and what it
//! has downloaded so far is in the store's temp folder. After a restart the
//! jobs that were working are queued again and continue from there.

use crate::db::PROVIDER_HF;
use crate::engine::Engine;
use crate::fetch::{Expect, NoSpace, Unavailable, Wait, WrongContent};
use crate::hub::{HubError, TreeEntry};
use crate::now;
use crate::pull::{self, PullRequest, PullWatch};
use anyhow::{Result, bail};
use dedupd_api::app::App;
use dedupd_api::jobs::{
    Downloads, FileStage, Job as JobSummary, JobAction, JobDetail, JobEvent, JobFile, JobKind,
    JobState, NotActivated, PullResult,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio::task::JoinHandle;

mod progress;
pub mod record;
mod run;

use progress::{JobWatch, Meter};
use record::{JobRecord, expect_of, file_state};
use run::wait_text;

/// Jobs that work at the same time. The governor decides how many
/// connections they share.
const MAX_ACTIVE: usize = 3;
/// Lines of a job's history that are kept.
const MAX_EVENTS: usize = 60;
/// Finished and cancelled jobs that are kept for the owner to look at.
const MAX_FINISHED: usize = 50;
/// Byte progress wakes the watchers at most this often.
const PROGRESS_TICK_MS: u64 = 250;

struct Job {
    record: Mutex<JobRecord>,
    task: Mutex<Option<JoinHandle<()>>>,
    /// Set when the owner stops the job, so a task that is being ended does
    /// not write a state after the one the owner chose.
    stopped: AtomicBool,
    meter: Meter,
}

impl Job {
    fn record(&self) -> std::sync::MutexGuard<'_, JobRecord> {
        self.record.lock().expect("job lock")
    }
}

/// The queue and what it is doing.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub governor: dedupd_api::jobs::Governor,
    /// Bytes per second of all jobs together.
    pub speed: u64,
    pub jobs: Vec<JobRecord>,
}

pub struct Queue {
    engine: Arc<Engine>,
    jobs: Mutex<BTreeMap<i64, Arc<Job>>>,
    version: watch::Sender<u64>,
    /// When byte progress last woke the watchers, in milliseconds.
    ticked: AtomicU64,
    started: Instant,
    meter: Meter,
    /// Called when a job is done, so the daemon can update the archive.
    on_done: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Queue {
    /// Open the queue. Jobs that were working when the daemon last stopped
    /// are queued again; `start` sets them going.
    pub fn open(engine: Arc<Engine>) -> Result<Arc<Self>> {
        let mut jobs = BTreeMap::new();
        let saved = engine.db().downloads()?;
        for (id, text) in saved {
            // A job saved by another build of dedupd, in a form this one
            // cannot read: it can be neither shown nor removed, and its
            // state would hold its model for ever, so it goes.
            let Ok(mut record) = serde_json::from_str::<JobRecord>(&text) else {
                engine.db().delete_download(id)?;
                continue;
            };
            if record.state.is_working() {
                record.state = JobState::Queued;
                record.detail = None;
                record.wait_until = None;
                record.note("continued after the daemon restarted");
            }
            record.speed = 0;
            jobs.insert(record.id, Arc::new(Job::new(record)));
        }
        Ok(Arc::new(Self {
            engine,
            jobs: Mutex::new(jobs),
            version: watch::Sender::new(0),
            ticked: AtomicU64::new(0),
            started: Instant::now(),
            meter: Meter::new(),
            on_done: Mutex::new(None),
        }))
    }

    /// Have `call` run whenever a job finishes.
    pub fn on_done(&self, call: impl Fn() + Send + Sync + 'static) {
        *self.on_done.lock().expect("lock") = Some(Arc::new(call));
    }

    /// Start the jobs that are waiting for their turn.
    pub fn start(self: &Arc<Self>) {
        let jobs = self.jobs.lock().expect("lock");
        let mut working = jobs
            .values()
            .filter(|j| j.record().state.is_working())
            .count();
        for job in jobs.values() {
            if working >= MAX_ACTIVE {
                break;
            }
            if job.record().state == JobState::Queued {
                self.spawn(job);
                working += 1;
            }
        }
    }

    fn spawn(self: &Arc<Self>, job: &Arc<Job>) {
        job.stopped.store(false, Ordering::Relaxed);
        self.set(job, JobState::Resolving, None);
        let (queue, running) = (self.clone(), job.clone());
        let handle = tokio::spawn(async move {
            queue.run(&running).await;
            queue.start();
        });
        *job.task.lock().expect("lock") = Some(handle);
    }

    /// Queue a pull of `model` (`org/repo[:quant][@revision]`).
    /// Queue a job, or name the one already queued for the same model,
    /// files, apps and source; that one is set going again when it was
    /// paused or waits for the owner. Returns its id and whether it was
    /// there already.
    pub fn add_or_find(
        self: &Arc<Self>,
        model: &str,
        include: Vec<String>,
        apps: Vec<App>,
        offline: bool,
        kind: JobKind,
    ) -> Result<(i64, bool)> {
        let wanted: BTreeSet<App> = apps.iter().copied().collect();
        let same = self.snapshot().jobs.into_iter().find(|job| {
            !job.state.is_finished()
                && job.model == model.trim()
                && job.include == include
                && job.offline == offline
                && job.apps.iter().copied().collect::<BTreeSet<_>>() == wanted
        });
        if let Some(job) = same {
            if matches!(job.state, JobState::Paused | JobState::NeedsAttention) {
                let _ = self.resume(job.id);
            }
            return Ok((job.id, true));
        }
        Ok((self.add(model, include, apps, offline, kind)?, false))
    }

    pub fn add(
        self: &Arc<Self>,
        model: &str,
        include: Vec<String>,
        apps: Vec<App>,
        offline: bool,
        kind: JobKind,
    ) -> Result<i64> {
        let req = PullRequest::from_spec(model, include, apps, offline, 1)?;
        let id = self
            .engine
            .db()
            .insert_download(JobState::Queued.as_str())?;
        let at = now();
        let mut record = JobRecord {
            id,
            model: model.trim().to_string(),
            repo: req.repo,
            revision: req.revision,
            quant: req.selection.quant,
            include: req.selection.include,
            apps: req.apps,
            offline,
            kind,
            state: JobState::Queued,
            detail: None,
            wait_until: None,
            commit: None,
            bytes_total: 0,
            bytes_done: 0,
            speed: 0,
            files: Vec::new(),
            events: Vec::new(),
            fetches: false,
            outcome: None,
            created_at: at,
            updated_at: at,
            finished_at: None,
        };
        record.note("queued");
        let job = Arc::new(Job::new(record));
        self.save(&job);
        self.jobs.lock().expect("lock").insert(id, job);
        self.changed();
        self.start();
        Ok(id)
    }

    fn job(&self, id: i64) -> Result<Arc<Job>> {
        match self.jobs.lock().expect("lock").get(&id) {
            Some(job) => Ok(job.clone()),
            None => bail!("there is no download {id}"),
        }
    }

    /// End the job's task, if one is running, and wait until it is gone.
    async fn stop(&self, job: &Job) {
        job.stopped.store(true, Ordering::Relaxed);
        let handle = job.task.lock().expect("lock").take();
        if let Some(handle) = handle {
            handle.abort();
            let _ = handle.await;
        }
    }

    /// Do what the owner asked to a job, when its state allows it.
    pub async fn act(self: &Arc<Self>, id: i64, action: JobAction) -> Result<()> {
        match action {
            JobAction::Pause => self.pause(id).await,
            JobAction::Resume | JobAction::Retry => self.resume(id),
            JobAction::Start => self.start_now(id),
            JobAction::Cancel => self.cancel(id).await,
            JobAction::Remove => self.remove(id),
        }
    }

    /// The job, when its state allows `actions`; the reason otherwise. The
    /// state decides, as `JobState::actions` lists it, so the app offers
    /// exactly what is allowed here.
    fn allowing(&self, id: i64, actions: &[JobAction]) -> Result<Arc<Job>> {
        let job = self.job(id)?;
        let state = job.record().state;
        if !actions.iter().any(|a| state.allows(*a)) {
            bail!(
                "download {id} is {}; it cannot be {} now",
                state.as_str().replace('_', " "),
                match actions[0] {
                    JobAction::Pause => "paused",
                    JobAction::Resume | JobAction::Retry => "continued",
                    JobAction::Start => "started ahead of the queue",
                    JobAction::Cancel => "cancelled",
                    JobAction::Remove => "removed",
                }
            );
        }
        Ok(job)
    }

    /// Stop a job; what it has downloaded is kept.
    pub async fn pause(self: &Arc<Self>, id: i64) -> Result<()> {
        let job = self.allowing(id, &[JobAction::Pause])?;
        self.stop(&job).await;
        {
            let mut r = job.record();
            r.state = JobState::Paused;
            r.detail = None;
            r.wait_until = None;
            r.speed = 0;
            r.note("paused");
        }
        self.save(&job);
        self.changed();
        self.start();
        Ok(())
    }

    /// Continue a paused job, or try again one that needed attention.
    pub fn resume(self: &Arc<Self>, id: i64) -> Result<()> {
        let job = self.allowing(id, &[JobAction::Resume, JobAction::Retry])?;
        {
            let mut r = job.record();
            r.state = JobState::Queued;
            r.detail = None;
            r.finished_at = None;
            r.note("queued again");
        }
        self.save(&job);
        self.changed();
        self.start();
        Ok(())
    }

    /// Start a queued job now, even when others are working.
    pub fn start_now(self: &Arc<Self>, id: i64) -> Result<()> {
        let job = self.allowing(id, &[JobAction::Start])?;
        self.spawn(&job);
        Ok(())
    }

    /// Stop a job and remove what it has downloaded so far.
    pub async fn cancel(self: &Arc<Self>, id: i64) -> Result<()> {
        let job = self.allowing(id, &[JobAction::Cancel])?;
        self.stop(&job).await;
        let files = job.record().files.clone();
        for file in files.iter().filter(|f| f.stage != FileStage::Stored) {
            let wanted = expect_of(file);
            if !self.wanted_elsewhere(id, &wanted) {
                self.engine.fetcher.discard(&self.engine.store, &wanted);
            }
        }
        {
            let mut r = job.record();
            r.state = JobState::Cancelled;
            r.detail = None;
            r.wait_until = None;
            r.speed = 0;
            r.finished_at = Some(now());
            r.note("cancelled");
        }
        self.save(&job);
        self.changed();
        self.start();
        Ok(())
    }

    /// Whether another job that is not finished needs the same file.
    fn wanted_elsewhere(&self, id: i64, wanted: &Expect) -> bool {
        let jobs = self.jobs.lock().expect("lock");
        jobs.iter().any(|(other, job)| {
            let r = job.record();
            *other != id
                && !r.state.is_finished()
                && r.files.iter().any(|f| expect_of(f) == *wanted)
        })
    }

    /// Take a finished job off the list.
    pub fn remove(&self, id: i64) -> Result<()> {
        self.allowing(id, &[JobAction::Remove])?;
        self.engine.db().delete_download(id)?;
        self.jobs.lock().expect("lock").remove(&id);
        self.changed();
        Ok(())
    }

    /// Take every finished and cancelled job off the list.
    pub fn clear_finished(&self) -> Result<usize> {
        let ids: Vec<i64> = {
            let jobs = self.jobs.lock().expect("lock");
            jobs.iter()
                .filter(|(_, j)| j.record().state.is_finished())
                .map(|(id, _)| *id)
                .collect()
        };
        for id in &ids {
            self.remove(*id)?;
        }
        Ok(ids.len())
    }

    /// A counter that goes up whenever anything about the queue changes.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }

    pub fn snapshot(&self) -> Snapshot {
        let jobs = self.jobs.lock().expect("lock");
        Snapshot {
            governor: self.engine.governor.state(),
            speed: self.meter.rate(),
            jobs: jobs
                .values()
                .map(|job| {
                    let mut record = job.record().clone();
                    record.speed = if record.state == JobState::Downloading {
                        job.meter.rate()
                    } else {
                        0
                    };
                    record
                })
                .collect(),
        }
    }

    /// The queue as the owner sees it.
    pub fn downloads(&self) -> Downloads {
        let snapshot = self.snapshot();
        Downloads {
            governor: snapshot.governor,
            speed: snapshot.speed,
            jobs: snapshot.jobs.iter().map(JobRecord::summary).collect(),
        }
    }

    pub fn get(&self, id: i64) -> Result<JobRecord> {
        Ok(self.job(id)?.record().clone())
    }

    /// Bytes the jobs that are working want and have.
    pub fn totals(&self) -> (u64, u64) {
        let jobs = self.jobs.lock().expect("lock");
        jobs.values().fold((0, 0), |(total, done), job| {
            let r = job.record();
            if r.state.is_working() {
                (total + r.bytes_total, done + r.bytes_done)
            } else {
                (total, done)
            }
        })
    }

    /// Wait until the job is done or has stopped for the owner.
    pub async fn settled(&self, id: i64) -> Result<JobRecord> {
        let mut changes = self.subscribe();
        loop {
            let record = self.get(id)?;
            if !record.state.is_working() && record.state != JobState::Queued {
                return Ok(record);
            }
            if changes.changed().await.is_err() {
                return Ok(record);
            }
        }
    }

    fn changed(&self) {
        self.version.send_modify(|v| *v += 1);
    }

    /// Wake the watchers for byte progress, but not for every chunk.
    fn ticked(&self) {
        let now = self.started.elapsed().as_millis() as u64;
        let last = self.ticked.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= PROGRESS_TICK_MS
            && self
                .ticked
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.changed();
        }
    }

    fn save(&self, job: &Job) {
        let (id, state, text) = {
            let mut r = job.record();
            r.updated_at = now();
            (
                r.id,
                r.state,
                serde_json::to_string(&*r).unwrap_or_default(),
            )
        };
        if let Err(e) = self.engine.db().save_download(id, state.as_str(), &text) {
            eprintln!("dedupd: could not save download {id}: {e:#}");
        }
    }

    /// Move a job to `state` from inside its own task.
    fn set(&self, job: &Job, state: JobState, detail: Option<String>) {
        if job.stopped.load(Ordering::Relaxed) {
            return;
        }
        {
            let mut r = job.record();
            if r.state == state && r.detail == detail {
                return;
            }
            r.state = state;
            if state != JobState::WaitingServer {
                r.wait_until = None;
            }
            let line = match &detail {
                Some(d) => format!("{}: {d}", state.as_str().replace('_', " ")),
                None => state.as_str().replace('_', " "),
            };
            r.detail = detail;
            r.note(line);
            if state.is_finished() || state == JobState::NeedsAttention {
                r.speed = 0;
            }
            if state.is_finished() {
                r.finished_at = Some(now());
            }
        }
        self.save(job);
        self.changed();
    }

    /// Keep the list of finished jobs from growing without end.
    fn forget_old(&self) {
        let old: Vec<i64> = {
            let jobs = self.jobs.lock().expect("lock");
            let finished: Vec<i64> = jobs
                .iter()
                .filter(|(_, j)| j.record().state.is_finished())
                .map(|(id, _)| *id)
                .collect();
            let extra = finished.len().saturating_sub(MAX_FINISHED);
            finished.into_iter().take(extra).collect()
        };
        for id in old {
            let _ = self.remove(id);
        }
    }
}

impl Job {
    fn new(record: JobRecord) -> Self {
        Self {
            record: Mutex::new(record),
            task: Mutex::new(None),
            stopped: AtomicBool::new(false),
            meter: Meter::new(),
        }
    }
}
