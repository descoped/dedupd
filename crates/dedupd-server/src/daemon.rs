//! The daemon's background work: notice what the apps downloaded on their
//! own, bring it into the store, free the duplicate space, and keep the
//! archive disk current.

use crate::Server;
use dedupd_core::adopt::{self, Action, Options};
use dedupd_core::place::App;
use dedupd_core::sources::LocalRepo;
use dedupd_core::store::Fingerprint;
use dedupd_core::{archive, maintain};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// A file must have been left alone this long before it is hashed, so a
/// download in progress is not picked up half-written.
const SETTLE: Duration = Duration::from_secs(20);

#[derive(Debug, Default, serde::Serialize)]
pub struct PassReport {
    /// App files that no longer exist and were dropped from the records.
    pub forgotten: usize,
    /// App files brought into the store in this pass.
    pub adopted: usize,
    pub adopted_bytes: u64,
    /// Duplicate app files swapped for clones.
    pub reclaimed: usize,
    pub reclaimed_bytes: u64,
    /// Files copied to the archive disk.
    pub archived: usize,
    pub errors: Vec<String>,
}

/// What the background pass keeps between rounds.
#[derive(Default)]
pub(crate) struct PassState {
    /// App files whose adoption failed, as they looked then. They are not
    /// hashed again until they change.
    failed_adoptions: Mutex<Seen>,
    /// Serialises the background pass and requests to run one now.
    lock: tokio::sync::Mutex<()>,
}

/// App files by path, as they looked when they were scanned.
type Seen = HashMap<String, Fingerprint>;

/// How long ago the file was last written. `None` when it cannot be read.
fn file_age(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    SystemTime::now().duration_since(modified).ok()
}

fn settled(path: &Path, settle: Duration) -> bool {
    file_age(path).is_some_and(|age| age >= settle)
}

/// One round of background work.
pub async fn pass(server: &Arc<Server>) -> anyhow::Result<PassReport> {
    pass_with(server, SETTLE, maintain::RECLAIM_MIN_AGE).await
}

/// `pass` with explicit waiting times, for tests.
pub async fn pass_with(
    server: &Arc<Server>,
    settle: Duration,
    reclaim_age: Duration,
) -> anyhow::Result<PassReport> {
    let _guard = server.passes.lock.lock().await;
    let mut report = PassReport::default();
    let (repos, scanned) = {
        let server = server.clone();
        tokio::task::spawn_blocking(move || new_files(&server, settle)).await??
    };
    report.forgotten = scanned.forgotten;
    adopt_new(server, repos, &scanned.files, &mut report).await?;
    if server.config.reclaim != Some(false) {
        reclaim_duplicates(server, reclaim_age, &mut report).await?;
    }
    update_archive(server, &mut report).await?;
    Ok(report)
}

/// What a look at the app folders found.
struct Scanned {
    /// Recorded app files that are gone and were dropped from the records.
    forgotten: usize,
    /// Every app file there is now.
    files: Seen,
}

/// The app files worth hashing: new or changed, not known to fail, and left
/// alone for `settle`. Only their repositories are worth asking Hugging Face
/// about.
fn new_files(server: &Server, settle: Duration) -> anyhow::Result<(Vec<LocalRepo>, Scanned)> {
    let engine = &server.engine;
    let forgotten = maintain::prune_placements(&engine.db)?;
    let mut repos = adopt::scan_apps(&engine.db, &engine.dirs, &App::ALL)?;
    let files: Seen = repos
        .iter()
        .flat_map(|r| &r.files)
        .map(|f| (f.abs_path.to_string_lossy().into_owned(), f.fingerprint))
        .collect();
    {
        let db = engine.db();
        let mut failed = server.passes.failed_adoptions.lock().expect("lock");
        // A file that failed and has changed since gets another try.
        failed.retain(|path, seen| files.get(path) == Some(seen));
        for repo in &mut repos {
            repo.files.retain(|f| {
                let path = f.abs_path.to_string_lossy();
                let known = db
                    .placement(&path)
                    .ok()
                    .flatten()
                    .is_some_and(|p| f.fingerprint == p.fingerprint());
                !known && !failed.contains_key(path.as_ref())
            });
        }
    }
    // The database is free again while the files are looked at.
    for repo in &mut repos {
        repo.files.retain(|f| settled(&f.abs_path, settle));
    }
    repos.retain(|r| !r.files.is_empty());
    Ok((repos, Scanned { forgotten, files }))
}

/// Bring the new files into the store, and remember the ones that failed as
/// they looked in `scanned`.
async fn adopt_new(
    server: &Server,
    repos: Vec<LocalRepo>,
    scanned: &Seen,
    report: &mut PassReport,
) -> anyhow::Result<()> {
    if repos.is_empty() {
        return Ok(());
    }
    let engine = &server.engine;
    let opts = Options {
        jobs: 2,
        ..Options::default()
    };
    let outcomes = adopt::adopt(
        &engine.db,
        &engine.store,
        Some(&engine.hub),
        repos,
        &opts,
        &mut |_| {},
    )
    .await?;
    for outcome in outcomes {
        for f in &outcome.files {
            if f.action != Action::Unchanged {
                report.adopted += 1;
                report.adopted_bytes += f.size;
            }
        }
        for (path, reason) in outcome.errors {
            if let Some(seen) = scanned.get(&path) {
                let mut failed = server.passes.failed_adoptions.lock().expect("lock");
                failed.insert(path.clone(), *seen);
            }
            report.errors.push(format!("{path}: {reason}"));
        }
    }
    Ok(())
}

/// Swap app files that duplicate stored content for clones.
async fn reclaim_duplicates(
    server: &Server,
    reclaim_age: Duration,
    report: &mut PassReport,
) -> anyhow::Result<()> {
    let engine = server.engine.clone();
    let done = tokio::task::spawn_blocking(move || {
        maintain::reclaim(&engine.db, &engine.store, reclaim_age, false)
    })
    .await??;
    report.reclaimed = done.swapped;
    report.reclaimed_bytes = done.bytes;
    Ok(())
}

/// Copy what the archive disk lacks, when it is attached.
async fn update_archive(server: &Server, report: &mut PassReport) -> anyhow::Result<()> {
    let engine = server.engine.clone();
    let synced = tokio::task::spawn_blocking(move || {
        archive::sync_if_attached(&engine.db, &engine.store, &mut |_, _| {})
    })
    .await?;
    match synced {
        Some(Ok(r)) => {
            report.archived = r.copied;
            for (sha, reason) in r.failed {
                report.errors.push(format!("archive {sha}: {reason}"));
            }
        }
        Some(Err(e)) => report.errors.push(format!("archive: {e:#}")),
        None => {}
    }
    Ok(())
}

/// Watch the app folders. Any change sends a unit on the returned channel.
/// The watcher must be kept alive for as long as events are wanted.
fn watch(
    server: &Server,
) -> Option<(
    notify::RecommendedWatcher,
    tokio::sync::mpsc::UnboundedReceiver<()>,
)> {
    use notify::Watcher;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok() {
            let _ = tx.send(());
        }
    })
    .ok()?;
    let dirs = &server.engine.dirs;
    let mut watching = 0;
    for dir in [&dirs.bionic, &dirs.ollama, &dirs.hf_cache] {
        // A folder that does not exist yet is covered by the periodic scan.
        if dir.is_dir() && watcher.watch(dir, notify::RecursiveMode::Recursive).is_ok() {
            watching += 1;
        }
    }
    (watching > 0).then_some((watcher, rx))
}

/// Hash the whole store once the last check is older than the configured
/// number of days, and write the result where the owner can read it.
async fn scheduled_verify(server: Arc<Server>) {
    let days = server.config.verify_days.unwrap_or(7);
    let marker = server.home.root().join("last-verify.txt");
    let due =
        days > 0 && file_age(&marker).is_none_or(|age| age >= Duration::from_secs(days * 86_400));
    if !due {
        return;
    }
    let engine = server.engine.clone();
    let done =
        tokio::task::spawn_blocking(move || maintain::verify(&engine.db, &engine.store, false, 2))
            .await;
    if let Ok(Ok(r)) = done {
        let summary = format!(
            "checked {} files: {} damaged, {} missing\n{}",
            r.checked,
            r.damaged.len(),
            r.missing.len(),
            r.damaged
                .iter()
                .map(|s| format!("DAMAGED {s}\n"))
                .chain(r.missing.iter().map(|s| format!("MISSING {s}\n")))
                .collect::<String>()
        );
        eprintln!(
            "dedupd: store check: {}",
            summary.lines().next().unwrap_or("")
        );
        let _ = std::fs::write(&marker, summary);
    }
}

/// Run the background work until the task is aborted: a pass shortly after
/// the app folders change, and one on a timer in case a change was missed.
pub async fn run(server: Arc<Server>) {
    let every = Duration::from_secs(server.config.scan_interval.unwrap_or(300).max(5));
    let mut watched = watch(&server);
    // The store check takes hours on a large store; the passes go on
    // meanwhile, and a second check never starts before the first has ended.
    let mut check: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        match pass(&server).await {
            Ok(r) => {
                if r.adopted + r.reclaimed + r.archived + r.forgotten > 0 {
                    eprintln!(
                        "dedupd: adopted {} files, swapped {} duplicates for clones, archived {}, forgot {} removed files",
                        r.adopted, r.reclaimed, r.archived, r.forgotten
                    );
                }
                for e in r.errors {
                    eprintln!("dedupd: {e}");
                }
            }
            Err(e) => eprintln!("dedupd: background pass failed: {e:#}"),
        }
        if check.as_ref().is_none_or(|c| c.is_finished()) {
            check = Some(tokio::spawn(scheduled_verify(server.clone())));
        }

        if watched.is_none() {
            watched = watch(&server);
        }
        let timer = tokio::time::sleep(every);
        tokio::pin!(timer);
        match watched.as_mut() {
            None => timer.await,
            Some((_, events)) => {
                tokio::select! {
                    _ = &mut timer => {}
                    _ = events.recv() => {
                        // Wait until the folders have been quiet long enough
                        // for the new files to count as settled.
                        let quiet = SETTLE + Duration::from_secs(2);
                        while let Ok(Some(())) =
                            tokio::time::timeout(quiet, events.recv()).await
                        {}
                    }
                }
            }
        }
    }
}
