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

/// Run the background work until the task is aborted.
pub async fn run(server: Arc<Server>) {
    let every = Duration::from_secs(server.config.scan_interval.unwrap_or(30).max(5));
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
        tokio::time::sleep(every).await;
    }
}
