//! What happened to the models in the apps: each activation, deactivation,
//! archive and restore, with its result in a sentence. The app shows it as
//! the Activity list.
//!
//! Kept in `<home>/activity.jsonl`, one entry per line, so the database
//! schema stays as it is and the command line and the daemon can both add
//! to it: a line is appended in one write. A line that cannot be read is
//! skipped; a missing file means nothing has happened yet. The file is cut
//! back to the newest entries when it grows long.
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::Home;
use crate::place::{App, Placed};

/// Entries kept when the file is cut back, and the length that starts it.
const KEEP: usize = 500;
const CUT_AT: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Activate,
    Deactivate,
    Archive,
    Restore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Grows with every entry: milliseconds since 1970, one more than the
    /// last entry when two come in the same millisecond.
    pub id: i64,
    /// Seconds since 1970.
    pub at: i64,
    pub action: Action,
    /// `org/repo`.
    pub repo: String,
    /// The app, for an activation or a deactivation.
    pub app: Option<String>,
    pub ok: bool,
    /// What happened, in a sentence; for a failure, why.
    pub text: String,
}

fn path(home: &Home) -> PathBuf {
    home.root().join("activity.jsonl")
}

fn read(home: &Home) -> Vec<Entry> {
    std::fs::read_to_string(path(home))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The newest entries first, at most `limit` of them.
pub fn recent(home: &Home, limit: usize) -> Vec<Entry> {
    let mut entries = read(home);
    entries.reverse();
    entries.truncate(limit);
    entries
}

/// Add an entry. A failure to write is reported, not passed on: the
/// history must not stop the work it describes.
pub fn record(
    home: &Home,
    action: Action,
    repo: &str,
    app: Option<&str>,
    result: std::result::Result<String, String>,
) {
    if let Err(e) = append(home, action, repo, app, result) {
        eprintln!("dedupd: could not add to the activity list: {e:#}");
    }
}

fn append(
    home: &Home,
    action: Action,
    repo: &str,
    app: Option<&str>,
    result: std::result::Result<String, String>,
) -> Result<()> {
    let known = read(home);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let millis = now.as_millis() as i64;
    let id = known.last().map_or(millis, |last| millis.max(last.id + 1));
    let (ok, text) = match result {
        Ok(text) => (true, text),
        Err(text) => (false, text),
    };
    let entry = Entry {
        id,
        at: now.as_secs() as i64,
        action,
        repo: repo.to_string(),
        app: app.map(str::to_string),
        ok,
        text,
    };
    let target = path(home);
    if known.len() + 1 >= CUT_AT {
        // Keep the newest entries and this one; the file is replaced in one
        // step.
        let mut lines = String::new();
        for e in known.iter().skip(known.len() + 1 - KEEP).chain([&entry]) {
            lines.push_str(&serde_json::to_string(e)?);
            lines.push('\n');
        }
        let tmp = home.root().join(".activity.jsonl.tmp");
        std::fs::write(&tmp, lines).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &target).with_context(|| format!("writing {}", target.display()))?;
        return Ok(());
    }
    let mut line = serde_json::to_string(&entry)?;
    line.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&target)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .with_context(|| format!("writing {}", target.display()))
}

/// Add one entry per app a pull was asked to put the model into: activated,
/// or why not. The reason comes from the pull's notes, which name the app.
pub fn record_activations(
    home: &Home,
    repo: &str,
    apps: &[App],
    placed: &[(App, Placed)],
    notes: &[String],
) {
    for app in apps {
        let took = placed
            .iter()
            .any(|(a, p)| a == app && p.conflicts.is_empty());
        let result = if took {
            Ok(format!("Activated in {}", app.title()))
        } else {
            Err(reason_for(*app, placed, notes))
        };
        record(home, Action::Activate, repo, Some(app.as_str()), result);
    }
}

/// Why `app` did not take the model, in a sentence.
fn reason_for(app: App, placed: &[(App, Placed)], notes: &[String]) -> String {
    let start = format!("not placed into {}: ", app.title());
    if let Some(why) = notes.iter().find_map(|n| n.strip_prefix(&start)) {
        return capital(why);
    }
    if let Some((_, p)) = placed.iter().find(|(a, _)| *a == app) {
        return format!(
            "{} files in its folder have other content; dedupd left them alone",
            p.conflicts.len()
        );
    }
    format!("{} did not take the model", capital(app.title()))
}

fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_entries_first_with_growing_ids() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        assert!(recent(&home, 10).is_empty());

        record(
            &home,
            Action::Activate,
            "org/a",
            Some("bionic"),
            Ok("Activated in Bionic".into()),
        );
        record(
            &home,
            Action::Activate,
            "org/a",
            Some("ollama"),
            Err("Ollama can't run it".into()),
        );
        record(&home, Action::Archive, "org/b", None, Ok("Archived".into()));

        let entries = recent(&home, 10);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].action, Action::Archive);
        assert_eq!(entries[0].app, None);
        assert!(!entries[1].ok);
        assert_eq!(entries[1].text, "Ollama can't run it");
        assert!(entries[0].id > entries[1].id && entries[1].id > entries[2].id);
        assert_eq!(recent(&home, 1).len(), 1);
    }

    #[test]
    fn a_damaged_line_is_skipped_and_a_long_file_is_cut_back() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        std::fs::write(path(&home), "not json\n").unwrap();
        record(&home, Action::Restore, "org/a", None, Ok("Restored".into()));
        assert_eq!(recent(&home, 10).len(), 1);

        for _ in 0..CUT_AT {
            record(
                &home,
                Action::Deactivate,
                "org/a",
                Some("hf"),
                Ok("Deactivated".into()),
            );
        }
        let entries = recent(&home, usize::MAX);
        assert!(
            entries.len() <= CUT_AT && entries.len() >= KEEP,
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
        let placed = vec![(
            App::Bionic,
            Placed {
                cloned: 2,
                ..Placed::default()
            },
        )];
        let notes = vec!["not placed into Ollama: Ollama 0.40.16 can't run this model: no.".into()];
        record_activations(
            &home,
            "org/a",
            &[App::Bionic, App::Ollama, App::Hf],
            &placed,
            &notes,
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
                (false, "The Hugging Face cache did not take the model"),
            ]
        );
        assert_eq!(entries[1].app.as_deref(), Some("ollama"));
    }
}
