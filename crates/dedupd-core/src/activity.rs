//! What happened to the models in the apps: each activation, deactivation,
//! archive and restore, with its result in a sentence. The app shows it as
//! the Activity list.
//!
//! Kept in `<home>/activity.jsonl`, one entry per line, so the database
//! schema stays as it is. The daemon and the command line both add to it,
//! one at a time: `<home>/activity.lock` is held while an entry is added,
//! which also keeps the ids unique. A line that cannot be read is skipped,
//! and one cut short by a crash does not take the next entry with it; a
//! missing file means nothing has happened yet. The file is cut back to the
//! newest entries when it grows long.
//!
//! The sentences are written here and nowhere else, and the operations that
//! change an app or the store add their own entries (`pull::place`,
//! `maintain::deactivate`, `archive::archive_apply`, `archive::restore`), so
//! every caller leaves the same history.
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::paths::Home;
use dedupd_api::activity::{ActivityAction, ActivityEntry};
use dedupd_api::app::App;
use dedupd_api::jobs::{NotActivated, PlacedApp};

/// Entries kept when the file is cut back, and the length that starts it.
const KEEP: usize = 500;
const CUT_AT: usize = 1000;

/// An entry before it has an id and a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Happened {
    pub action: ActivityAction,
    pub repo: String,
    pub app: Option<App>,
    /// What happened, or why it did not.
    pub result: std::result::Result<String, String>,
}

/// The sentences of the list.
pub fn activated(app: App) -> String {
    format!("Activated in {}", app.title())
}

pub fn deactivated(app: App) -> String {
    format!("Deactivated in {}", app.title())
}

pub fn partly_deactivated(app: App, removed: usize, left: usize) -> String {
    format!(
        "Partly deactivated in {}: {removed} files removed, {left} left there",
        app.title()
    )
}

pub const ARCHIVED: &str = "Moved to the archive disk";
pub const RESTORED: &str = "Brought back from the archive disk";

fn path(home: &Home) -> PathBuf {
    home.root().join("activity.jsonl")
}

fn lock_path(home: &Home) -> PathBuf {
    home.root().join("activity.lock")
}

fn parse(text: &str) -> Vec<ActivityEntry> {
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The newest entries first, at most `limit` of them.
pub fn recent(home: &Home, limit: usize) -> Vec<ActivityEntry> {
    let mut entries = parse(&std::fs::read_to_string(path(home)).unwrap_or_default());
    entries.reverse();
    entries.truncate(limit);
    entries
}

/// Add one entry. A failure to write is reported, not passed on: the
/// history must not stop the work it describes.
pub fn record(home: &Home, happened: Happened) {
    record_all(home, vec![happened]);
}

/// Add entries, in this order, in one go.
pub fn record_all(home: &Home, happened: Vec<Happened>) {
    if happened.is_empty() {
        return;
    }
    if let Err(e) = append(home, happened) {
        eprintln!("dedupd: could not add to the activity list: {e:#}");
    }
}

/// Add the outcome of an operation: `ok` words a success, an error is its
/// own reason.
pub fn record_result<T>(
    home: &Home,
    action: ActivityAction,
    repo: &str,
    app: Option<App>,
    result: &Result<T>,
    ok: impl FnOnce(&T) -> Option<String>,
) {
    let result = match result {
        Ok(value) => match ok(value) {
            Some(text) => Ok(text),
            // Nothing happened, so there is nothing to tell.
            None => return,
        },
        Err(e) => Err(format!("{e:#}")),
    };
    record(
        home,
        Happened {
            action,
            repo: repo.to_string(),
            app,
            result,
        },
    );
}

fn append(home: &Home, happened: Vec<Happened>) -> Result<()> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(home))
        .context("opening the activity lock")?;
    lock.lock().context("waiting for the activity lock")?;

    let target = path(home);
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&target)
        .with_context(|| format!("opening {}", target.display()))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .with_context(|| format!("reading {}", target.display()))?;
    let known = parse(&text);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // One more than the last entry, or the time now when that is later.
    let first = known
        .last()
        .map_or(0, |last| last.id)
        .max(now.as_millis() as i64 - 1)
        + 1;
    let mut lines = String::new();
    for (id, h) in (first..).zip(happened) {
        let (ok, text) = match h.result {
            Ok(text) => (true, text),
            Err(text) => (false, text),
        };
        let entry = ActivityEntry {
            id,
            at: now.as_secs() as i64,
            action: h.action,
            repo: h.repo,
            app: h.app,
            ok,
            text,
        };
        lines.push_str(&serde_json::to_string(&entry)?);
        lines.push('\n');
    }

    if known.len() + lines.lines().count() >= CUT_AT {
        // Keep the newest entries and the new ones; the file is replaced in
        // one step, under the lock.
        let mut kept = String::new();
        for e in known.iter().skip(known.len().saturating_sub(KEEP)) {
            kept.push_str(&serde_json::to_string(e)?);
            kept.push('\n');
        }
        kept.push_str(&lines);
        let tmp = home
            .root()
            .join(format!(".activity.jsonl.{}.tmp", std::process::id()));
        std::fs::write(&tmp, kept).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &target).with_context(|| format!("writing {}", target.display()))?;
        return Ok(());
    }
    // A line cut short by a crash gets its end, so the new entries start on
    // a line of their own.
    if !text.is_empty() && !text.ends_with('\n') {
        lines.insert(0, '\n');
    }
    file.seek(SeekFrom::End(0))?;
    file.write_all(lines.as_bytes())
        .with_context(|| format!("writing {}", target.display()))?;
    // The lock is let go when `lock` is closed, after the write.
    drop(file);
    drop(lock);
    Ok(())
}

/// Add one entry per app a pull was asked to put the model into: activated,
/// or why not.
pub fn record_activations(
    home: &Home,
    repo: &str,
    apps: &[App],
    placed: &[PlacedApp],
    skipped: &[NotActivated],
) {
    let happened = apps
        .iter()
        .map(|app| {
            let result = match skipped.iter().find(|s| s.app == *app) {
                Some(s) => Err(s.reason.clone()),
                None if placed.iter().any(|p| p.app == *app) => Ok(activated(*app)),
                None => Err("no reason was given".to_string()),
            };
            Happened {
                action: ActivityAction::Activate,
                repo: repo.to_string(),
                app: Some(*app),
                result,
            }
        })
        .collect();
    record_all(home, happened);
}

/// An activation that stopped before anything was placed: one entry per
/// app, with why.
pub fn record_stopped_activation(home: &Home, repo: &str, apps: &[App], why: &str) {
    let happened = apps
        .iter()
        .map(|app| Happened {
            action: ActivityAction::Activate,
            repo: repo.to_string(),
            app: Some(*app),
            result: Err(why.to_string()),
        })
        .collect();
    record_all(home, happened);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn happened(
        action: ActivityAction,
        app: Option<App>,
        result: std::result::Result<&str, &str>,
    ) -> Happened {
        Happened {
            action,
            repo: "org/a".into(),
            app,
            result: result.map(str::to_string).map_err(str::to_string),
        }
    }

    #[test]
    fn keeps_the_newest_entries_first_with_growing_ids() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        assert!(recent(&home, 10).is_empty());

        record(
            &home,
            happened(
                ActivityAction::Activate,
                Some(App::Bionic),
                Ok("Activated in Bionic"),
            ),
        );
        record(
            &home,
            happened(
                ActivityAction::Activate,
                Some(App::Ollama),
                Err("Ollama can't run it"),
            ),
        );
        record(&home, happened(ActivityAction::Archive, None, Ok(ARCHIVED)));

        let entries = recent(&home, 10);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].action, ActivityAction::Archive);
        assert_eq!(entries[0].app, None);
        assert_eq!(entries[1].app, Some(App::Ollama));
        assert!(!entries[1].ok);
        assert_eq!(entries[1].text, "Ollama can't run it");
        assert!(entries[0].id > entries[1].id && entries[1].id > entries[2].id);
        assert_eq!(recent(&home, 1).len(), 1);
    }

    #[test]
    fn writers_at_the_same_time_lose_nothing_and_share_no_id() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..25 {
                        record(
                            &home,
                            happened(ActivityAction::Deactivate, Some(App::Hf), Ok("Deactivated")),
                        );
                    }
                });
            }
        });
        let entries = recent(&home, usize::MAX);
        assert_eq!(entries.len(), 200);
        let mut ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), 200);
    }

    #[test]
    fn a_damaged_line_is_skipped_and_does_not_take_the_next_entry() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        // A write cut short: no end of line.
        std::fs::write(path(&home), "not json\n{\"id\":17,\"at\"").unwrap();
        record(&home, happened(ActivityAction::Restore, None, Ok(RESTORED)));
        let entries = recent(&home, 10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, RESTORED);
    }

    #[test]
    fn a_long_file_is_cut_back_to_the_newest_entries() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        for _ in 0..CUT_AT {
            record(
                &home,
                happened(ActivityAction::Deactivate, Some(App::Hf), Ok("Deactivated")),
            );
        }
        let entries = recent(&home, usize::MAX);
        assert!(
            entries.len() < CUT_AT && entries.len() > KEEP,
            "{}",
            entries.len()
        );
        let ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
        assert!(ids.windows(2).all(|w| w[0] > w[1]));
    }

    #[test]
    fn an_activation_says_which_apps_took_the_model_and_why_not() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        let placed = vec![
            PlacedApp {
                app: App::Bionic,
                cloned: 2,
                present: 0,
                conflicts: vec![],
            },
            PlacedApp {
                app: App::Hf,
                cloned: 0,
                present: 0,
                conflicts: vec!["x".into()],
            },
        ];
        let skipped = vec![
            NotActivated {
                app: App::Ollama,
                reason: "Ollama 0.40.16 can't run this model: no.".into(),
                temporary: false,
            },
            NotActivated {
                app: App::Hf,
                reason: "files in its folder with other content were left alone (1)".into(),
                temporary: false,
            },
        ];
        record_activations(
            &home,
            "org/a",
            &[App::Bionic, App::Ollama, App::Hf],
            &placed,
            &skipped,
        );

        let entries = recent(&home, 10);
        let texts: Vec<(bool, &str)> = entries
            .iter()
            .rev()
            .map(|e| (e.ok, e.text.as_str()))
            .collect();
        assert_eq!(
            texts,
            [
                (true, "Activated in Bionic"),
                (false, "Ollama 0.40.16 can't run this model: no."),
                (
                    false,
                    "files in its folder with other content were left alone (1)"
                ),
            ]
        );
    }

    #[test]
    fn a_result_without_anything_done_is_not_told() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        let nothing: Result<usize> = Ok(0);
        record_result(
            &home,
            ActivityAction::Deactivate,
            "org/a",
            Some(App::Bionic),
            &nothing,
            |n| (*n > 0).then(|| deactivated(App::Bionic)),
        );
        let failed: Result<usize> = Err(anyhow::anyhow!("removing x: permission denied"));
        record_result(
            &home,
            ActivityAction::Deactivate,
            "org/a",
            Some(App::Bionic),
            &failed,
            |_| None,
        );
        let entries = recent(&home, 10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "removing x: permission denied");
    }
}
