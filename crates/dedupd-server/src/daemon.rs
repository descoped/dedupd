//! The daemon's background work: notice what the apps downloaded on their
//! own, bring it into the store, free the duplicate space, and keep the
//! archive disk current.

use crate::Server;
use dedupd_core::adopt::{self, Action, Options};
use dedupd_core::place::App;
use dedupd_core::store::Fingerprint;
use dedupd_core::{archive, maintain};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// A file must have been left alone this long before it is hashed, so a
/// download in progress is not picked up half-written.
const SETTLE: Duration = Duration::from_secs(20);
/// A duplicate must have been left alone this long before it is swapped for
/// a clone.
const RECLAIM_AGE: Duration = Duration::from_secs(60);

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

fn settled(path: &std::path::Path, settle: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age >= settle)
}

/// One round of background work.
pub async fn pass(server: &Arc<Server>) -> anyhow::Result<PassReport> {
    pass_with(server, SETTLE, RECLAIM_AGE).await
}

/// `pass` with explicit waiting times, for tests.
pub async fn pass_with(
    server: &Arc<Server>,
    settle: Duration,
    reclaim_age: Duration,
) -> anyhow::Result<PassReport> {
    let _guard = server.pass_lock.lock().await;
    let engine = &server.engine;
    let mut report = PassReport {
        forgotten: maintain::prune_placements(&engine.db)?,
        ..Default::default()
    };

    // Only repositories with a new or changed file are worth asking
    // Hugging Face about.
    let mut repos = adopt::scan_apps(&engine.db, &engine.dirs, &App::ALL)?;
    {
        let db = engine.db();
        for repo in &mut repos {
            repo.files.retain(|f| {
                let known = db
                    .placement(&f.abs_path.to_string_lossy())
                    .ok()
                    .flatten()
                    .is_some_and(|p| {
                        f.fingerprint
                            == Fingerprint {
                                size: p.size,
                                mtime_ns: p.mtime_ns,
                            }
                    });
                !known && settled(&f.abs_path, settle)
            });
        }
    }
    repos.retain(|r| !r.files.is_empty());
    if !repos.is_empty() {
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
                report.errors.push(format!("{path}: {reason}"));
            }
        }
    }

    if server.config.reclaim != Some(false) {
        let engine = engine.clone();
        let done = tokio::task::spawn_blocking(move || {
            maintain::reclaim(&engine.db, &engine.store, reclaim_age, false)
        })
        .await??;
        report.reclaimed = done.swapped;
        report.reclaimed_bytes = done.bytes;
    }

    if let Some(archive_home) = server.config.archive_home()
        && archive::attached(&archive_home)
    {
        let engine = engine.clone();
        let synced = tokio::task::spawn_blocking(move || {
            archive::sync(&engine.db, &engine.store, &archive_home, &mut |_, _| {})
        })
        .await?;
        match synced {
            Ok(r) => {
                report.archived = r.copied;
                for (sha, reason) in r.failed {
                    report.errors.push(format!("archive {sha}: {reason}"));
                }
            }
            Err(e) => report.errors.push(format!("archive: {e:#}")),
        }
    }
    Ok(report)
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
async fn scheduled_verify(server: &Arc<Server>) {
    let days = server.config.verify_days.unwrap_or(7);
    let marker = server.home.root().join("last-verify.txt");
    let due = days > 0
        && std::fs::metadata(&marker)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_none_or(|age| age >= Duration::from_secs(days * 86_400));
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
        scheduled_verify(&server).await;

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
