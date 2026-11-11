//! The daemon's download queue.
//!
//! A job is a pull that can be watched, paused, continued and cancelled, and
//! that survives a restart: every job is a row in the database, and what it
//! has downloaded so far is in the store's temp folder. After a restart the
//! jobs that were working are queued again and continue from there.

use crate::db::PROVIDER_HF;
use crate::fetch::{Expect, NoSpace, Unavailable, Wait, WrongContent};
use crate::governor::GovernorState;
use crate::hub::{HubError, TreeEntry};
use crate::now;
use crate::place::App;
use crate::pull::{self, Engine, PullRequest, PullWatch};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Jobs that work at the same time. The governor decides how many
/// connections they share.
const MAX_ACTIVE: usize = 3;
/// Lines of a job's history that are kept.
const MAX_EVENTS: usize = 60;
/// Finished and cancelled jobs that are kept for the owner to look at.
const MAX_FINISHED: usize = 50;
/// Byte progress wakes the watchers at most this often.
const PROGRESS_TICK_MS: u64 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
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

impl State {
    pub const fn as_str(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Resolving => "resolving",
            State::Downloading => "downloading",
            State::Verifying => "verifying",
            State::Placing => "placing",
            State::Done => "done",
            State::Paused => "paused",
            State::WaitingServer => "waiting_server",
            State::WaitingNetwork => "waiting_network",
            State::NeedsAttention => "needs_attention",
            State::Cancelled => "cancelled",
        }
    }

    /// A task is working on the job.
    pub const fn is_working(self) -> bool {
        matches!(
            self,
            State::Resolving
                | State::Downloading
                | State::Verifying
                | State::Placing
                | State::WaitingServer
                | State::WaitingNetwork
        )
    }

    /// Nothing more will happen to the job.
    pub const fn is_finished(self) -> bool {
        matches!(self, State::Done | State::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStage {
    Waiting,
    Downloading,
    Verifying,
    Stored,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    pub path: String,
    pub size: u64,
    /// Bytes on disk.
    pub done: u64,
    pub stage: FileStage,
    /// sha256 of the content, for files the provider keeps in LFS.
    pub sha256: Option<String>,
    pub git_oid: String,
}

impl FileState {
    fn expect(&self) -> Expect {
        match &self.sha256 {
            Some(sha) => Expect::Sha256(sha.clone()),
            None => Expect::GitOid(self.git_oid.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Seconds since the Unix epoch.
    pub at: i64,
    pub text: String,
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
    pub state: State,
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
    pub files: Vec<FileState>,
    pub events: Vec<Event>,
    /// What the pull did, once it is done.
    pub outcome: Option<serde_json::Value>,
    pub created_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
}

impl JobRecord {
    fn request(&self, jobs: usize) -> Result<PullRequest> {
        PullRequest::from_spec(
            &self.model,
            self.include.clone(),
            self.apps.clone(),
            self.offline,
            jobs,
        )
    }

    fn note(&mut self, text: impl Into<String>) {
        self.events.push(Event {
            at: now(),
            text: text.into(),
        });
        if self.events.len() > MAX_EVENTS {
            let extra = self.events.len() - MAX_EVENTS;
            self.events.drain(..extra);
        }
    }
}

/// A rate measured from a running count.
struct Meter {
    count: AtomicU64,
    sample: Mutex<(Instant, u64, u64)>,
}

impl Meter {
    fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            sample: Mutex::new((Instant::now(), 0, 0)),
        }
    }

    fn add(&self, bytes: u64) {
        self.count.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Bytes per second since the last look that was at least a second ago.
    fn rate(&self) -> u64 {
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

/// The queue and what it is doing, for the owner to look at.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub governor: GovernorState,
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

/// What stopped an attempt, by what to do about it.
enum Problem {
    Network(String),
    Busy(u64),
    /// Only the owner can do something about it. `gone` when the provider
    /// said it does not have the repository, or will not show it.
    Owner {
        why: String,
        gone: bool,
    },
}

impl Queue {
    /// Open the queue. Jobs that were working when the daemon last stopped
    /// are queued again; `start` sets them going.
    pub fn open(engine: Arc<Engine>) -> Result<Arc<Self>> {
        let mut jobs = BTreeMap::new();
        for text in engine.db().downloads()? {
            let Ok(mut record) = serde_json::from_str::<JobRecord>(&text) else {
                continue;
            };
            if record.state.is_working() {
                record.state = State::Queued;
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
            if job.record().state == State::Queued {
                self.spawn(job);
                working += 1;
            }
        }
    }

    fn spawn(self: &Arc<Self>, job: &Arc<Job>) {
        job.stopped.store(false, Ordering::Relaxed);
        self.set(job, State::Resolving, None);
        let (queue, running) = (self.clone(), job.clone());
        let handle = tokio::spawn(async move {
            queue.run(&running).await;
            queue.start();
        });
        *job.task.lock().expect("lock") = Some(handle);
    }

    /// Queue a pull of `model` (`org/repo[:quant][@revision]`).
    pub fn add(
        self: &Arc<Self>,
        model: &str,
        include: Vec<String>,
        apps: Vec<App>,
        offline: bool,
    ) -> Result<i64> {
        let req = PullRequest::from_spec(model, include, apps, offline, 1)?;
        let id = self.engine.db().insert_download(State::Queued.as_str())?;
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
            state: State::Queued,
            detail: None,
            wait_until: None,
            commit: None,
            bytes_total: 0,
            bytes_done: 0,
            speed: 0,
            files: Vec::new(),
            events: Vec::new(),
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

    /// Stop a job; what it has downloaded is kept.
    pub async fn pause(self: &Arc<Self>, id: i64) -> Result<()> {
        let job = self.job(id)?;
        if job.record().state.is_finished() {
            bail!("download {id} is already finished");
        }
        self.stop(&job).await;
        {
            let mut r = job.record();
            r.state = State::Paused;
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
            let mut r = job.record();
            if !matches!(
                r.state,
                State::Paused | State::NeedsAttention | State::Cancelled
            ) {
                bail!("download {id} is not stopped");
            }
            r.state = State::Queued;
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
        let job = self.job(id)?;
        if job.record().state != State::Queued {
            bail!("download {id} is not waiting for its turn");
        }
        self.spawn(&job);
        Ok(())
    }

    /// Stop a job and remove what it has downloaded so far.
    pub async fn cancel(self: &Arc<Self>, id: i64) -> Result<()> {
        let job = self.job(id)?;
        if job.record().state.is_finished() {
            bail!("download {id} is already finished");
        }
        self.stop(&job).await;
        let files = job.record().files.clone();
        for file in files.iter().filter(|f| f.stage != FileStage::Stored) {
            let wanted = file.expect();
            if !self.wanted_elsewhere(id, &wanted) {
                self.engine.fetcher.discard(&self.engine.store, &wanted);
            }
        }
        {
            let mut r = job.record();
            r.state = State::Cancelled;
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
            *other != id && !r.state.is_finished() && r.files.iter().any(|f| f.expect() == *wanted)
        })
    }

    /// Take a job that is not working off the list.
    pub fn remove(&self, id: i64) -> Result<()> {
        let job = self.job(id)?;
        if job.record().state.is_working() {
            bail!("download {id} is working; cancel it first");
        }
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
                    record.speed = if record.state == State::Downloading {
                        job.meter.rate()
                    } else {
                        0
                    };
                    record
                })
                .collect(),
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
            if !record.state.is_working() && record.state != State::Queued {
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
    fn set(&self, job: &Job, state: State, detail: Option<String>) {
        if job.stopped.load(Ordering::Relaxed) {
            return;
        }
        {
            let mut r = job.record();
            if r.state == state && r.detail == detail {
                return;
            }
            r.state = state;
            if state != State::WaitingServer {
                r.wait_until = None;
            }
            let line = match &detail {
                Some(d) => format!("{}: {d}", state.as_str().replace('_', " ")),
                None => state.as_str().replace('_', " "),
            };
            r.detail = detail;
            r.note(line);
            if state.is_finished() || state == State::NeedsAttention {
                r.speed = 0;
            }
            if state.is_finished() {
                r.finished_at = Some(now());
            }
        }
        self.save(job);
        self.changed();
    }

    async fn run(self: &Arc<Self>, job: &Arc<Job>) {
        let engine = &self.engine;
        let jobs = engine.governor.max_connections();
        let (req, pinned) = {
            let r = job.record();
            match r.request(jobs) {
                Ok(req) => (req, r.commit.clone()),
                Err(e) => {
                    drop(r);
                    return self.set(job, State::NeedsAttention, Some(format!("{e:#}")));
                }
            }
        };

        // Settle the revision and the files, waiting out a provider that is
        // busy or cannot be reached.
        let mut failures = 0u32;
        let plan = loop {
            self.set(job, State::Resolving, None);
            let problem = match pull::plan(engine, &req, pinned.as_deref()).await {
                Ok(plan) => break plan,
                Err(e) => problem_of(&e, &req),
            };
            failures += 1;
            match problem {
                Problem::Owner { why, gone } => {
                    if gone {
                        let _ = engine.db().set_upstream(PROVIDER_HF, &req.repo, true);
                    }
                    return self.set(job, State::NeedsAttention, Some(why));
                }
                Problem::Busy(seconds) => {
                    job.record().wait_until = Some(now() + seconds as i64);
                    self.set(job, State::WaitingServer, Some(wait_text(seconds)));
                    tokio::time::sleep(Duration::from_secs(seconds)).await;
                }
                Problem::Network(why) => {
                    self.set(job, State::WaitingNetwork, Some(why));
                    let pause = Duration::from_secs(2u64.pow(failures.min(5)).min(30));
                    tokio::time::sleep(pause).await;
                }
            }
        };
        if !plan.from_archive {
            let _ = engine.db().set_upstream(PROVIDER_HF, &req.repo, false);
        }
        {
            let mut r = job.record();
            r.commit = Some(plan.snap.commit.clone());
            r.bytes_total = plan.bytes;
            r.bytes_done = 0;
            r.files = plan.entries.iter().map(file_state).collect();
            let from = if plan.from_archive {
                ", from the archive"
            } else {
                ""
            };
            r.note(format!(
                "settled on {}@{}, {} files{from}",
                req.revision,
                &plan.snap.commit[..12.min(plan.snap.commit.len())],
                plan.entries.len()
            ));
        }
        self.set(job, State::Downloading, None);

        let watch = Arc::new(JobWatch {
            queue: self.clone(),
            job: job.clone(),
        });
        let fetched = pull::fetch(engine, &req, &plan, watch).await;
        let (items, downloaded) = match fetched {
            Ok(done) => done,
            Err(e) => {
                let why = attention_text(&pull::gone_upstream(e, &req, &plan), &req);
                return self.set(job, State::NeedsAttention, Some(why));
            }
        };

        self.set(job, State::Placing, None);
        let files = items.len();
        match pull::place(engine, &req, &plan, items).await {
            Ok((placed, notes)) => {
                let outcome = pull::outcome(&req, plan, files, downloaded, placed, notes);
                {
                    let mut r = job.record();
                    r.bytes_done = r.bytes_total;
                    r.outcome = serde_json::to_value(&outcome).ok();
                }
                self.set(job, State::Done, None);
                self.forget_old();
                let call = self.on_done.lock().expect("lock").clone();
                if let Some(call) = call {
                    call();
                }
            }
            Err(e) => self.set(job, State::NeedsAttention, Some(format!("{e:#}"))),
        }
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

fn file_state(entry: &TreeEntry) -> FileState {
    FileState {
        path: entry.path.clone(),
        size: entry.size,
        done: 0,
        stage: FileStage::Waiting,
        sha256: entry.lfs_sha256.clone(),
        git_oid: entry.git_oid.clone(),
    }
}

fn wait_text(seconds: u64) -> String {
    format!("Hugging Face asked for a pause of {seconds} s")
}

/// Why the revision could not be settled.
fn problem_of(e: &anyhow::Error, req: &PullRequest) -> Problem {
    match e.downcast_ref::<HubError>() {
        Some(HubError::Busy { seconds }) => Problem::Busy(*seconds),
        Some(HubError::Transport(_)) => {
            Problem::Network("Hugging Face cannot be reached".to_string())
        }
        Some(HubError::Status { status, .. }) if *status >= 500 => {
            Problem::Network(format!("Hugging Face answered HTTP {status}"))
        }
        Some(HubError::NotAvailable { status, .. }) => Problem::Owner {
            why: not_available(*status, req),
            gone: true,
        },
        _ => Problem::Owner {
            why: format!("{e:#}"),
            gone: false,
        },
    }
}

/// What to tell the owner when the provider will not hand something over.
fn not_available(status: u16, req: &PullRequest) -> String {
    match status {
        401 | 403 => format!(
            "Hugging Face will not show {} to this Mac (HTTP {status}). If the model is \
             gated, accept its terms on huggingface.co and make sure a token is set, then retry.",
            req.repo
        ),
        _ => format!(
            "{} is not on Hugging Face (HTTP {status}), and it is not archived here.",
            req.repo
        ),
    }
}

/// Why the files could not be fetched, for the owner.
fn attention_text(e: &anyhow::Error, req: &PullRequest) -> String {
    if let Some(unavailable) = e.downcast_ref::<Unavailable>() {
        return not_available(unavailable.status, req);
    }
    if let Some(space) = e.downcast_ref::<NoSpace>() {
        return format!("{space}. Free some space, then retry.");
    }
    if e.downcast_ref::<WrongContent>().is_some() {
        return "A file arrived damaged twice. Retry later; if it keeps happening the file \
                on Hugging Face does not match its listing."
            .to_string();
    }
    format!("{e:#}")
}

/// Carries what the fetcher reports into the job's record.
struct JobWatch {
    queue: Arc<Queue>,
    job: Arc<Job>,
}

impl PullWatch for JobWatch {
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
            delta > 0 && matches!(r.state, State::WaitingServer | State::WaitingNetwork)
        };
        if resumed {
            self.queue.set(&self.job, State::Downloading, None);
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
                    .set(&self.job, State::WaitingServer, Some(wait_text(seconds)));
            }
            Wait::Network => self.queue.set(
                &self.job,
                State::WaitingNetwork,
                Some("the download server cannot be reached".to_string()),
            ),
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
            self.queue.set(&self.job, State::Verifying, None);
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
