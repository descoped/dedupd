//! What an app said when dedupd asked it about a model and it said no.
//!
//! Some answers can only be had by asking the app: Ollama is shown a
//! stand-in of an MLX model before the model is placed, and may refuse it
//! (an architecture it does not run, for one). Its answer holds for that
//! model and that version of the app, so it is kept and the model is shown
//! as not usable there, instead of offering a placement that fails again.
//! A newer version of the app is asked again.
//!
//! Kept in `<home>/refusals.json` rather than the database, so the schema
//! stays as it is. A missing or unreadable file means nothing is known.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::Home;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    /// The app, as its profile names it.
    pub app: String,
    /// The version of the app that was asked.
    pub version: String,
    /// The method that was tried.
    pub method: String,
    /// The sha256 of the model's `config.json`: the same model in another
    /// repository gets the same answer, and a changed one is asked again.
    pub config: String,
    /// Why, in one sentence.
    pub reason: String,
    /// When, in seconds since 1970.
    pub at: i64,
}

#[derive(Debug, Default)]
pub struct Refusals {
    list: Vec<Refusal>,
}

fn path(home: &Home) -> PathBuf {
    home.root().join("refusals.json")
}

impl Refusals {
    /// What is known in `home`; nothing when the file is missing or cannot
    /// be read.
    pub fn load(home: &Home) -> Self {
        let list = std::fs::read(path(home))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { list }
    }

    /// The answer `version` of `app` gave for the model with this
    /// `config.json`, by `method`.
    pub fn find(&self, app: &str, version: &str, method: &str, config: &str) -> Option<&Refusal> {
        self.list.iter().find(|r| {
            r.app == app && r.version == version && r.method == method && r.config == config
        })
    }

    /// Keep `refusal` in `home`, in place of an earlier answer to the same
    /// question. The file is replaced in one step.
    pub fn record(home: &Home, refusal: Refusal) -> Result<()> {
        let mut known = Self::load(home);
        known.list.retain(|r| {
            (&r.app, &r.version, &r.method, &r.config)
                != (
                    &refusal.app,
                    &refusal.version,
                    &refusal.method,
                    &refusal.config,
                )
        });
        known.list.push(refusal);
        let target = path(home);
        let tmp = home.root().join(".refusals.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&known.list)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &target).with_context(|| format!("writing {}", target.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(version: &str, reason: &str) -> Refusal {
        Refusal {
            app: "ollama".into(),
            version: version.into(),
            method: "clone-with-rewritten-index".into(),
            config: "c0".into(),
            reason: reason.into(),
            at: 1,
        }
    }

    #[test]
    fn keeps_one_answer_per_question() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        assert!(
            Refusals::load(&home)
                .find("ollama", "0.40.16", "clone-with-rewritten-index", "c0")
                .is_none()
        );

        Refusals::record(&home, refusal("0.40.16", "first")).unwrap();
        Refusals::record(&home, refusal("0.40.16", "second")).unwrap();
        Refusals::record(&home, refusal("0.41.0", "other")).unwrap();
        let known = Refusals::load(&home);
        let method = "clone-with-rewritten-index";
        assert_eq!(
            known
                .find("ollama", "0.40.16", method, "c0")
                .unwrap()
                .reason,
            "second"
        );
        assert_eq!(
            known.find("ollama", "0.41.0", method, "c0").unwrap().reason,
            "other"
        );
        assert!(known.find("ollama", "0.40.16", method, "c1").is_none());
        assert!(known.find("bionic", "0.40.16", method, "c0").is_none());
    }

    #[test]
    fn an_unreadable_file_means_nothing_is_known() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        std::fs::write(dir.path().join("refusals.json"), b"not json").unwrap();
        assert!(Refusals::load(&home).list.is_empty());
        Refusals::record(&home, refusal("0.40.16", "after")).unwrap();
        assert_eq!(Refusals::load(&home).list.len(), 1);
    }
}
