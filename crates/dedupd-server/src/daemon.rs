//! The daemon's background work: notice what the apps downloaded on their
//! own, bring it into the store, free the duplicate space, and keep the
//! archive disk current.

use crate::{Server, epoch, held};
use dedupd_api::app::App;
use dedupd_api::daemon::{ScanResult, StoreCheck};
use dedupd_core::adopt::{self, Action, Options};
use dedupd_core::sources::LocalRepo;
use dedupd_core::store::Fingerprint;
use dedupd_core::{archive, maintain};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// A file must have been left alone this long before it is hashed, so a
/// download in progress is not picked up half-written.
const SETTLE: Duration = Duration::from_secs(20);

/// What the background pass keeps between rounds.
#[derive(Default)]
pub(crate) struct PassState {
    /// App files whose adoption failed, as they looked then. They are not
    /// hashed again until they change.
    failed_adoptions: Mutex<Seen>,
    /// Store files the database does not know that were read and left out
    /// (damaged, or nothing refers to them), by sha256, as they looked
    /// then. They are not read again until they change.
    store_looked_at: Mutex<Seen>,
    /// App files dated after now, as they looked at the last pass. Such a
    /// file's age says nothing; it counts as left alone once it has not
    /// changed from one pass to the next.
    dated_ahead: Mutex<Seen>,
    /// What the last pass could not do, as it was logged: the same failure
    /// is said once, not on every pass.
    said: Mutex<BTreeSet<String>>,
    /// Serialises the background pass and requests to run one now.
    lock: tokio::sync::Mutex<()>,
}

/// Files by path or sha256, as they looked when they were looked at.
type Seen = HashMap<String, Fingerprint>;

/// How long ago the file was last written. `None` when it cannot be read,
/// and nothing at all for a file dated after now (a clock that was set
/// back, a file copied from another machine).
fn file_age(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    Some(
        SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::ZERO),
    )
}

fn dated_ahead(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|modified| modified > SystemTime::now())
}

fn settled(path: &Path, settle: Duration) -> bool {
    file_age(path).is_some_and(|age| age >= settle)
}

/// Log what a pass could not do. A failure that lasts (a full archive
/// disk, a folder that cannot be read) is said when it first appears and
/// again when it comes back, not every few minutes.
pub(crate) fn say(server: &Server, errors: &[String]) {
    let mut said = held(&server.passes.said);
    for e in errors {
        if !said.contains(e) {
            eprintln!("dedupd: {e}");
        }
    }
    *said = errors.iter().cloned().collect();
}

/// One round of background work.
pub async fn pass(server: &Arc<Server>) -> anyhow::Result<ScanResult> {
    pass_with(server, SETTLE, maintain::RECLAIM_MIN_AGE).await
}

/// `pass` with explicit waiting times, for tests.
pub async fn pass_with(
    server: &Arc<Server>,
    settle: Duration,
    reclaim_age: Duration,
) -> anyhow::Result<ScanResult> {
    let _guard = server.passes.lock.lock().await;
    // Files a client began to download through the daemon and left.
    {
        let server = server.clone();
        tokio::task::spawn_blocking(move || {
            server.drop_idle_assemblies(crate::files::ASSEMBLY_IDLE)
        })
        .await?;
    }
    // Each step stands by itself: what one could not do goes into the
    // report, and the steps after it are still taken. A folder of one app
    // that cannot be read does not keep the archive disk from its update.
    let mut report = ScanResult::default();
    let (repos, scanned) = {
        let server = server.clone();
        tokio::task::spawn_blocking(move || new_files(&server, settle)).await?
    };
    report.forgotten = scanned.forgotten as u64;
    report.errors.extend(scanned.errors.iter().cloned());
    if let Err(e) = adopt_new(server, repos, &scanned.files, &mut report).await {
        report.errors.push(format!("adoption: {e:#}"));
    }
    if let Err(e) = recognise_stored(server, &mut report).await {
        report
            .errors
            .push(format!("the store could not be looked through: {e:#}"));
    }
    if server.config.reclaim()
        && let Err(e) = reclaim_duplicates(server, reclaim_age, &mut report).await
    {
        report.errors.push(format!("reclaim: {e:#}"));
    }
    update_archive(server, &mut report).await?;
    if report.forgotten + report.adopted + report.recognised + report.reclaimed + report.archived
        > 0
    {
        server.touch();
    }
    crate::favorites::refresh_token(server);
    crate::favorites::sync_if_due(server);
    Ok(report)
}

/// Record the files the store holds and the database does not know.
async fn recognise_stored(server: &Arc<Server>, report: &mut ScanResult) -> anyhow::Result<()> {
    let server = server.clone();
    let found = tokio::task::spawn_blocking(move || {
        let mut looked_at = held(&server.passes.store_looked_at);
        adopt::recognise_stored(&server.engine.db, &server.engine.store, &mut looked_at)
    })
    .await??;
    report.recognised = found.files;
    // Said once: a damaged file is not read again until it changes.
    if found.damaged > 0 {
        report.errors.push(format!(
            "{} in the store {} not match {} name; `dedupd verify` checks the store",
            if found.damaged == 1 {
                "1 file".to_string()
            } else {
                format!("{} files", found.damaged)
            },
            if found.damaged == 1 { "does" } else { "do" },
            if found.damaged == 1 { "its" } else { "their" },
        ));
    }
    Ok(())
}

/// What a look at the app folders found.
struct Scanned {
    /// Recorded app files that are gone and were dropped from the records.
    forgotten: usize,
    /// Every app file there is now.
    files: Seen,
    /// What could not be looked at, in sentences.
    errors: Vec<String>,
}

/// The app files worth hashing: new or changed, not known to fail, and left
/// alone for `settle`. Only their repositories are worth asking Hugging Face
/// about.
fn new_files(server: &Server, settle: Duration) -> (Vec<LocalRepo>, Scanned) {
    let engine = &server.engine;
    let mut errors = Vec::new();
    let forgotten = maintain::prune_placements(&engine.db).unwrap_or_else(|e| {
        errors.push(format!("the records of the apps' files: {e:#}"));
        0
    });
    // One app at a time: a folder that cannot be read is that app's.
    let mut repos = Vec::new();
    for app in App::ALL {
        match adopt::scan_apps(&engine.db, &engine.dirs, &[app]) {
            Ok(found) => repos.extend(found),
            Err(e) => errors.push(format!(
                "the folder of {} could not be read: {e:#}",
                app.as_str()
            )),
        }
    }
    let files: Seen = repos
        .iter()
        .flat_map(|r| &r.files)
        .map(|f| (f.abs_path.to_string_lossy().into_owned(), f.fingerprint))
        .collect();
    {
        let db = engine.db();
        let mut failed = held(&server.passes.failed_adoptions);
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
    let mut ahead = held(&server.passes.dated_ahead);
    let mut ahead_now = Seen::new();
    for repo in &mut repos {
        repo.files.retain(|f| {
            if !dated_ahead(&f.abs_path) {
                return settled(&f.abs_path, settle);
            }
            let path = f.abs_path.to_string_lossy().into_owned();
            let unchanged = ahead.get(&path) == Some(&f.fingerprint);
            ahead_now.insert(path, f.fingerprint);
            unchanged
        });
    }
    *ahead = ahead_now;
    drop(ahead);
    repos.retain(|r| !r.files.is_empty());
    (
        repos,
        Scanned {
            forgotten,
            files,
            errors,
        },
    )
}

/// Bring the new files into the store, and remember the ones that failed as
/// they looked in `scanned`.
async fn adopt_new(
    server: &Server,
    repos: Vec<LocalRepo>,
    scanned: &Seen,
    report: &mut ScanResult,
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
                let mut failed = held(&server.passes.failed_adoptions);
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
    report: &mut ScanResult,
) -> anyhow::Result<()> {
    let engine = server.engine.clone();
    let done = tokio::task::spawn_blocking(move || {
        maintain::reclaim(&engine.db, &engine.store, reclaim_age, false)
    })
    .await??;
    report.reclaimed = done.swapped as u64;
    report.reclaimed_bytes = done.bytes;
    Ok(())
}

/// Copy what the archive disk lacks, when it is attached.
async fn update_archive(server: &Server, report: &mut ScanResult) -> anyhow::Result<()> {
    let engine = server.engine.clone();
    let synced = tokio::task::spawn_blocking(move || {
        archive::sync_if_attached(&engine.db, &engine.store, &mut |_, _| {})
    })
    .await?;
    match synced {
        Some(Ok(r)) => {
            report.archived = r.copied as u64;
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

/// Hash the whole store and write the result where the owner can read it.
/// Does nothing when a check is already running.
pub async fn check_store(server: Arc<Server>) {
    {
        let mut check = held(&server.check);
        if check.running {
            return;
        }
        *check = StoreCheck {
            running: true,
            started_at: Some(epoch()),
            ..StoreCheck::default()
        };
    }
    server.touch();
    let engine = server.engine.clone();
    let done =
        tokio::task::spawn_blocking(move || maintain::verify(&engine.db, &engine.store, false, 2))
            .await;
    {
        let mut check = held(&server.check);
        check.running = false;
        check.finished_at = Some(epoch());
        match done {
            Ok(Ok(r)) => {
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
                // Without the note the next check is due at once; the
                // time of this one is also kept in memory (`check_due`).
                if let Err(e) = std::fs::write(server.home.last_verify_path(), summary) {
                    eprintln!("dedupd: the store check's note could not be written: {e}");
                }
                check.checked = r.checked as u64;
                check.bytes = r.bytes;
                check.only_in_archive = r.only_in_archive as u64;
                check.archive_away = r.archive_away as u64;
                check.damaged = r.damaged;
                check.missing = r.missing;
                // Not damaged, and not checked either: said as it is.
                if let Some((sha, why)) = r.unreadable.first() {
                    check.error = Some(format!(
                        "{} of the files could not be read, so nothing is known about them (the first: {}: {why})",
                        r.unreadable.len(),
                        sha.get(..12).unwrap_or(sha)
                    ));
                }
            }
            Ok(Err(e)) => check.error = Some(format!("{e:#}")),
            Err(e) => check.error = Some(e.to_string()),
        }
        if let Some(why) = &check.error {
            eprintln!("dedupd: store check: {why}");
        }
    }
    server.touch();
}

/// When the store was last checked, from the note the check leaves.
pub fn last_check(server: &Server) -> Option<i64> {
    let modified = std::fs::metadata(server.home.last_verify_path())
        .and_then(|m| m.modified())
        .ok()?;
    Some(
        modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64,
    )
}

/// Check the store once the last check is older than the configured number
/// of days.
async fn scheduled_verify(server: Arc<Server>) {
    let days = server.config.verify_days();
    let marker = server.home.last_verify_path();
    let due =
        days > 0 && file_age(&marker).is_none_or(|age| age >= Duration::from_secs(days * 86_400));
    // A check that failed left no note. It is tried again a day later, not
    // after every pass: a failure late in a large store would otherwise
    // have the whole store hashed again and again.
    let tried_lately = held(&server.check)
        .finished_at
        .is_some_and(|at| epoch().saturating_sub(at) < 86_400);
    if due && !tried_lately {
        check_store(server).await;
    }
}

/// Run the background work until the task is aborted: a pass shortly after
/// the app folders change, and one on a timer in case a change was missed.
pub async fn run(server: Arc<Server>) {
    let every = Duration::from_secs(server.config.scan_interval().max(5));
    let mut watched = watch(&server);
    // The store check takes hours on a large store; the passes go on
    // meanwhile, and a second check never starts before the first has ended.
    let mut check: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        match pass(&server).await {
            Ok(r) => {
                if r.adopted + r.recognised + r.reclaimed + r.archived + r.forgotten > 0 {
                    eprintln!(
                        "dedupd: adopted {} files, recognised {} stored files, swapped {} duplicates for clones, archived {}, forgot {} removed files",
                        r.adopted, r.recognised, r.reclaimed, r.archived, r.forgotten
                    );
                }
                say(&server, &r.errors);
            }
            Err(e) => say(&server, &[format!("background pass failed: {e:#}")]),
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
                    first = events.recv() => {
                        // Wait until the folders have been quiet long enough
                        // for the new files to count as settled, but never
                        // longer than the timer would have: an app that
                        // writes for an hour does not hold off the passes,
                        // which skip the files still being written.
                        let quiet = SETTLE + Duration::from_secs(2);
                        let latest = tokio::time::Instant::now() + every.max(quiet);
                        let mut open = first.is_some();
                        while open {
                            let left = latest.saturating_duration_since(tokio::time::Instant::now());
                            if left.is_zero() {
                                break;
                            }
                            match tokio::time::timeout(quiet.min(left), events.recv()).await {
                                Ok(Some(())) => {}
                                Ok(None) => open = false,
                                Err(_) => break,
                            }
                        }
                        // The watcher is gone: watch again, and wait for
                        // the timer meanwhile.
                        if !open {
                            watched = None;
                            timer.await;
                        }
                    }
                }
            }
        }
    }
}
