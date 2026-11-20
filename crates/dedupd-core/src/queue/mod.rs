//! The daemon's download queue.
//!
//! A job is a pull that can be watched, paused, continued and cancelled, and
//! that survives a restart: every job is a row in the database, and what it
//! has downloaded so far is in the store's temp folder. After a restart the
//! jobs that were working are queued again and continue from there.

use crate::engine::Engine;
use crate::fetch::Expect;
use crate::now;
use crate::pull::PullRequest;
use anyhow::Result;
use dedupd_api::app::App;
use dedupd_api::jobs::{Downloads, FileStage, JobAction, JobKind, JobState};
use futures_util::FutureExt;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio::task::JoinHandle;

mod progress;
pub mod record;
mod run;

use progress::Meter;
use record::{JobRecord, expect_of};

/// Why the queue did not do what it was asked. The control API answers
/// each kind with a status of its own; anything else the queue returns is
/// a failure of the daemon.
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error("there is no download {0}")]
    NoSuchJob(i64),
    /// The job's state does not allow the action, in a sentence.
    #[error("{0}")]
    NotAllowed(String),
}

/// Jobs that work at the same time. The governor decides how many
/// connections they share.
const MAX_ACTIVE: usize = 3;
/// The longest an owner's action waits for a stopped job's downloads to end.
const STOP_WAIT: Duration = Duration::from_secs(10);
/// Lines of a job's history that are kept.
const MAX_EVENTS: usize = 60;
/// Finished and cancelled jobs that are kept for the owner to look at.
const MAX_FINISHED: usize = 50;
/// Byte progress wakes the watchers at most this often.
const PROGRESS_TICK_MS: u64 = 250;

struct Job {
    record: Mutex<JobRecord>,
    /// Held while the record is written to the database, so two saves of
    /// one job land in the order they read the record: the row never goes
    /// back to an older state.
    saving: Mutex<()>,
    task: Mutex<Option<JoinHandle<()>>>,
    /// Set when the owner stops the job, so a task that is being ended does
    /// not write a state after the one the owner chose.
    stopped: AtomicBool,
    /// How many Pause or Cancel actions are between their check and their
    /// last word. While one is, the job is not queued again: the files it
    /// is about to clean up would be those of a task that runs anew.
    settling: AtomicUsize,
    meter: Meter,
    /// Held, as a second reference, by the job's file downloads while any
    /// of them is alive. Ending the job's task ends them too, but each
    /// stops in its own time; `stop` waits here until all are gone, so
    /// nothing writes a file after the owner's action has cleaned up.
    alive: Arc<()>,
}

/// A job a Pause or Cancel has claimed, until the action has had its last
/// word (also when it ends early).
struct Settling(Arc<Job>);

impl std::ops::Deref for Settling {
    type Target = Arc<Job>;
    fn deref(&self) -> &Arc<Job> {
        &self.0
    }
}

impl Drop for Settling {
    fn drop(&mut self) {
        self.0.settling.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Job {
    /// The record, also after a task panicked while it held the lock: a
    /// record is plain data and every change to it is whole in itself, so
    /// what is there can be shown, and the job can be tried again.
    fn record(&self) -> std::sync::MutexGuard<'_, JobRecord> {
        self.record.lock().unwrap_or_else(|e| e.into_inner())
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

/// The download queue: every job the daemon has, the tasks that run them,
/// and what the owner can do to each. One per daemon, shared by `Arc`;
/// every method can be called from any task. The owner's actions go
/// through [`Queue::act`].
pub struct Queue {
    engine: Arc<Engine>,
    jobs: Mutex<BTreeMap<i64, Arc<Job>>>,
    /// Held while a job is looked for and, when there is none, added: two
    /// equal requests at the same moment give one job.
    adding: Mutex<()>,
    version: watch::Sender<u64>,
    /// When byte progress last woke the watchers, in milliseconds.
    ticked: AtomicU64,
    started: Instant,
    meter: Meter,
    /// Called when a job is done or a run put files into apps, so the
    /// daemon can say the library changed and update the archive.
    on_placed: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
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
            // state would hold its model for ever, so it goes. The log
            // says so, with the model when that much can be read.
            let mut record = match serde_json::from_str::<JobRecord>(&text) {
                Ok(record) => record,
                Err(e) => {
                    let model = serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .and_then(|v| v["model"].as_str().map(str::to_string))
                        .map_or_else(String::new, |m| format!(" ({m:?})"));
                    eprintln!(
                        "dedupd: removed download {id}{model}, saved by another build of dedupd: {e}"
                    );
                    engine.db().delete_download(id)?;
                    continue;
                }
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
            adding: Mutex::new(()),
            version: watch::Sender::new(0),
            ticked: AtomicU64::new(0),
            started: Instant::now(),
            meter: Meter::new(),
            on_placed: Mutex::new(None),
        }))
    }

    /// Have `call` run when a job is done (its files are stored, so the
    /// archive has something to take, whether or not an app took the
    /// model), and when a run put the model into at least one app and the
    /// job then waits for the owner because another app could not be asked.
    pub fn on_placed(&self, call: impl Fn() + Send + Sync + 'static) {
        *self.on_placed.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(call));
    }

    /// The list of jobs, also after a task panicked while it held the
    /// lock: the map is only added to and removed from, one step each.
    fn list(&self) -> std::sync::MutexGuard<'_, BTreeMap<i64, Arc<Job>>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start the jobs that are waiting for their turn.
    pub fn start(self: &Arc<Self>) {
        let started: Vec<Arc<Job>> = {
            let jobs = self.list();
            let mut working = jobs
                .values()
                .filter(|j| j.record().state.is_working())
                .count();
            let mut started = Vec::new();
            for job in jobs.values() {
                if working >= MAX_ACTIVE {
                    break;
                }
                if job.record().state == JobState::Queued {
                    self.spawn(job);
                    started.push(job.clone());
                    working += 1;
                }
            }
            started
        };
        // The database is written after the list is given back: `snapshot`
        // needs the list for every line of the event stream.
        self.started(&started);
    }

    /// Give a queued job its task. Only called with the `jobs` lock held
    /// and the job seen as queued under it, as `claim` also holds it: a job
    /// is started once, and never while an owner's action stops it. The
    /// caller writes the new state down once the lock is given back
    /// (`started`).
    ///
    /// The task owns the job to its end: when `run` panics, the job waits
    /// for the owner with a sentence, the log names the job and the model,
    /// and the next job gets its turn all the same.
    fn spawn(self: &Arc<Self>, job: &Arc<Job>) {
        job.stopped.store(false, Ordering::SeqCst);
        {
            let mut r = job.record();
            r.state = JobState::Resolving;
            r.detail = None;
            r.wait_until = None;
            r.note("resolving");
        }
        let (queue, running) = (self.clone(), job.clone());
        let handle = tokio::spawn(async move {
            let ran = std::panic::AssertUnwindSafe(queue.run(&running))
                .catch_unwind()
                .await;
            if let Err(panic) = ran {
                let what = panic
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "no message".to_string());
                let (id, model) = {
                    let r = running.record();
                    (r.id, r.model.clone())
                };
                eprintln!("dedupd: download {id} ({model}) stopped on a fault in dedupd: {what}");
                queue.set(
                    &running,
                    JobState::NeedsAttention,
                    Some("dedupd ran into a fault while working on this; try again".to_string()),
                );
            }
            queue.start();
        });
        *job.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    /// Write down that these jobs were started, and tell the watchers.
    fn started(&self, jobs: &[Arc<Job>]) {
        for job in jobs {
            self.save(job);
        }
        if !jobs.is_empty() {
            self.changed();
        }
    }

    /// Queue a job, or name the one already queued for the same model,
    /// files, apps and source; that one is set going again when it was
    /// paused or waits for the owner. Returns its id and whether it was
    /// there already. Looking and adding are one step: two equal requests
    /// at the same moment get the same job.
    pub fn add_or_find(
        self: &Arc<Self>,
        model: &str,
        include: Vec<String>,
        apps: Vec<App>,
        offline: bool,
        kind: JobKind,
    ) -> Result<(i64, bool)> {
        let _one = self.adding.lock().unwrap_or_else(|e| e.into_inner());
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
                // It fails only when the job's state changed in between
                // (the owner acted, or it started by itself): the job is
                // there either way, and that is the answer.
                let _ = self.resume(job.id);
            }
            return Ok((job.id, true));
        }
        Ok((self.add(model, include, apps, offline, kind)?, false))
    }

    /// Queue a job for `model` (`org/repo[:quant][@revision]`), also when
    /// an equal one is queued already, and return its id. The job is saved
    /// before this returns and starts when its turn comes. Fails for a
    /// name that is not a model's, or when the database cannot be written.
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
        self.list().insert(id, job);
        self.changed();
        self.start();
        Ok(id)
    }

    fn job(&self, id: i64) -> Result<Arc<Job>> {
        match self.list().get(&id) {
            Some(job) => Ok(job.clone()),
            None => Err(QueueError::NoSuchJob(id).into()),
        }
    }

    /// The first half of Pause and Cancel, in one step with the check: the
    /// job when its state allows `action`, marked as stopped so its task
    /// writes no state after this, with the task to end. A job that waits
    /// for its turn is taken out of the turn here. `start` and `start_now`
    /// hold the same lock, so neither can start the job in between.
    fn claim(&self, id: i64, action: JobAction) -> Result<(Settling, Option<JoinHandle<()>>)> {
        let jobs = self.list();
        let Some(job) = jobs.get(&id).cloned() else {
            return Err(QueueError::NoSuchJob(id).into());
        };
        {
            let mut r = job.record();
            if !r.state.allows(action) {
                return Err(QueueError::NotAllowed(not_now(id, r.state, action)).into());
            }
            // Under the record's lock, which `set` holds to look at the
            // mark: a task that is about to place sees it and ends.
            job.stopped.store(true, Ordering::SeqCst);
            job.settling.fetch_add(1, Ordering::SeqCst);
            if r.state == JobState::Queued {
                r.state = JobState::Paused;
            }
        }
        let handle = job.task.lock().unwrap_or_else(|e| e.into_inner()).take();
        Ok((Settling(job), handle))
    }

    /// End the job's task, if one is running, and wait until it is gone.
    async fn stop(&self, job: &Job, handle: Option<JoinHandle<()>>) {
        if let Some(handle) = handle {
            handle.abort();
            let _ = handle.await;
        }
        // The file downloads the task started are ended with it and stop at
        // their next step. That takes a moment; an hour-long hash on a
        // blocking thread is not waited for.
        let deadline = Instant::now() + STOP_WAIT;
        while Arc::strong_count(&job.alive) > 1 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Do what the owner asked to a job, when its state allows it. The
    /// work runs on a task of its own, so a caller that goes away (a
    /// request whose client hung up) cannot cut it in two.
    pub async fn act(self: &Arc<Self>, id: i64, action: JobAction) -> Result<()> {
        let queue = self.clone();
        tokio::spawn(async move { queue.act_now(id, action).await })
            .await
            .map_err(|e| anyhow::anyhow!("the action on download {id} did not finish: {e}"))?
    }

    async fn act_now(self: &Arc<Self>, id: i64, action: JobAction) -> Result<()> {
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
            return Err(QueueError::NotAllowed(not_now(id, state, actions[0])).into());
        }
        Ok(job)
    }

    /// Stop a job; what it has downloaded is kept. (The daemon goes
    /// through `act`; this and the actions below are public for the tests,
    /// which call one without a task between.)
    pub async fn pause(self: &Arc<Self>, id: i64) -> Result<()> {
        let (job, handle) = self.claim(id, JobAction::Pause)?;
        self.stop(&job, handle).await;
        {
            let mut r = job.record();
            // A Cancel that came at the same time has the last word.
            if r.state.is_finished() {
                return Ok(());
            }
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
        let job = self.job(id)?;
        {
            // The check and the change under one lock.
            let mut r = job.record();
            if !(r.state.allows(JobAction::Resume) || r.state.allows(JobAction::Retry)) {
                return Err(QueueError::NotAllowed(not_now(id, r.state, JobAction::Resume)).into());
            }
            // A Pause or Cancel that has claimed the job (under this lock)
            // and has not written its last word yet goes first.
            if job.settling.load(Ordering::SeqCst) > 0 {
                return Err(QueueError::NotAllowed(format!(
                    "download {id} is being stopped; try again in a moment"
                ))
                .into());
            }
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
        let job = {
            let jobs = self.list();
            let Some(job) = jobs.get(&id) else {
                return Err(QueueError::NoSuchJob(id).into());
            };
            let state = job.record().state;
            if !state.allows(JobAction::Start) {
                return Err(QueueError::NotAllowed(not_now(id, state, JobAction::Start)).into());
            }
            self.spawn(job);
            job.clone()
        };
        self.started(&[job]);
        Ok(())
    }

    /// Stop a job and remove what it has downloaded so far.
    pub async fn cancel(self: &Arc<Self>, id: i64) -> Result<()> {
        let (job, handle) = self.claim(id, JobAction::Cancel)?;
        self.stop(&job, handle).await;
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
        let jobs = self.list();
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
        self.list().remove(&id);
        self.changed();
        Ok(())
    }

    /// Take every finished and cancelled job off the list.
    pub fn clear_finished(&self) -> Result<usize> {
        let ids: Vec<i64> = {
            let jobs = self.list();
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

    /// The queue at this moment: every job's record with its speed, the
    /// governor's state and the speed of all jobs together. A copy; it
    /// does not change as the queue goes on.
    pub fn snapshot(&self) -> Snapshot {
        // Before the list is taken: the governor has a lock of its own.
        let governor = self.engine.governor.state();
        let jobs = self.list();
        Snapshot {
            governor,
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

    /// One job's record as it is now; `QueueError::NoSuchJob` when there
    /// is none with this id.
    pub fn get(&self, id: i64) -> Result<JobRecord> {
        Ok(self.job(id)?.record().clone())
    }

    /// Bytes the jobs that are working want and have.
    pub fn totals(&self) -> (u64, u64) {
        let jobs = self.list();
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

    /// Write the job's record to its row. Called from whichever thread
    /// changed the record, also a worker of the async runtime: the write is
    /// one row in a database in WAL mode, behind a lock every other holder
    /// takes for one statement at a time, so it is short. It is never made
    /// while the queue's list is held.
    fn save(&self, job: &Job) {
        let _in_turn = job.saving.lock().unwrap_or_else(|e| e.into_inner());
        let (id, state, text) = {
            let mut r = job.record();
            r.updated_at = now();
            (r.id, r.state, serde_json::to_string(&*r))
        };
        // An empty text would be read as a job of another build at the
        // next start, and the job removed: better the row as it was.
        let text = match text {
            Ok(text) => text,
            Err(e) => {
                eprintln!("dedupd: could not write download {id} down: {e}");
                return;
            }
        };
        if let Err(e) = self.engine.db().save_download(id, state.as_str(), &text) {
            eprintln!("dedupd: could not save download {id}: {e:#}");
        }
    }

    /// Move a job to `state` from inside its own task. False when the owner
    /// has stopped the job: nothing is written, and the task must not begin
    /// anything that cannot be cut off.
    fn set(&self, job: &Job, state: JobState, detail: Option<String>) -> bool {
        {
            let mut r = job.record();
            // Looked at under the record's lock, as `claim` sets it.
            if job.stopped.load(Ordering::SeqCst) {
                return false;
            }
            if r.state == state && r.detail == detail {
                return true;
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
        true
    }

    /// Keep the list of finished jobs from growing without end.
    fn forget_old(&self) {
        let old: Vec<i64> = {
            let jobs = self.list();
            let finished: Vec<i64> = jobs
                .iter()
                .filter(|(_, j)| j.record().state.is_finished())
                .map(|(id, _)| *id)
                .collect();
            let extra = finished.len().saturating_sub(MAX_FINISHED);
            finished.into_iter().take(extra).collect()
        };
        for id in old {
            // One the owner removed in between is gone already.
            if let Err(e) = self.remove(id)
                && !matches!(e.downcast_ref(), Some(QueueError::NoSuchJob(_)))
            {
                eprintln!("dedupd: could not take download {id} off the list: {e:#}");
            }
        }
    }
}

/// Why `action` cannot be done to a job in `state`.
fn not_now(id: i64, state: JobState, action: JobAction) -> String {
    format!(
        "download {id} is {}; it cannot be {} now",
        state.as_str().replace('_', " "),
        match action {
            JobAction::Pause => "paused",
            JobAction::Resume | JobAction::Retry => "continued",
            JobAction::Start => "started ahead of the queue",
            JobAction::Cancel => "cancelled",
            JobAction::Remove => "removed",
        }
    )
}

impl Job {
    fn new(record: JobRecord) -> Self {
        Self {
            record: Mutex::new(record),
            saving: Mutex::new(()),
            task: Mutex::new(None),
            stopped: AtomicBool::new(false),
            settling: AtomicUsize::new(0),
            meter: Meter::new(),
            alive: Arc::new(()),
        }
    }
}
