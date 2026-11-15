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
use dedupd_api::app::App;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub app: App,
    /// The version of the app that was asked.
    pub version: String,
    /// The method that was tried.
    pub method: String,
    /// The sha256 of the model's `config.json`: the same model in another
    /// repository gets the same answer, and a changed one is asked again.
    pub config: String,
    /// What the app said, in its own words.
    pub reason: String,
    /// Checked to be about the model when it was kept. Answers kept by older
    /// builds are not, and count only if what they say passes the check now.
    #[serde(default)]
    pub lasting: bool,
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
        let list: Vec<Refusal> = std::fs::read(path(home))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let list = list
            .into_iter()
            .filter(|r| r.lasting || is_about_the_model(own_words(&r.reason)))
            .collect();
        Self { list }
    }

    /// The answer `version` of `app` gave for the model with this
    /// `config.json`, by `method`.
    pub fn find(&self, app: App, version: &str, method: &str, config: &str) -> Option<&Refusal> {
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

/// What older builds put in front of Ollama's words when they kept them.
const OLDER_WORDS: &str = "Ollama would not import a stand-in for the model: ";

/// What the app said, without the words older builds put in front.
fn own_words(reason: &str) -> &str {
    reason.strip_prefix(OLDER_WORDS).unwrap_or(reason).trim()
}

/// Whether what Ollama said when it would not import the model is about the
/// model, and so holds until Ollama changes: worth keeping. Only Ollama's
/// own phrases about a model's kind count; anything else (a full disk, a
/// crash, a signal, "operation not supported" from the system) is about the
/// moment, and the next activation asks again.
pub fn is_about_the_model(said: &str) -> bool {
    let said = said.to_lowercase();
    [
        "unsupported mlx architecture",
        "unsupported architecture",
        "unknown architecture",
        "unknown model architecture",
        "unsupported model architecture",
        "unsupported quantization",
        "unsupported data type",
        "unknown data type",
        "unsupported dtype",
        "unknown dtype",
    ]
    .iter()
    .any(|words| said.contains(words))
}

/// Why `app` at `version` cannot run the model, in a sentence a person
/// reads: "Ollama 0.40.16 can't run this model: it doesn't support the
/// model type DeepseekV4ForCausalLM for MLX models."
pub fn sentence(app: &str, version: &str, said: &str) -> String {
    let said = own_words(said).trim_end_matches('.');
    let why = match architecture(said) {
        Some(kind) => format!("it doesn't support the model type {kind} for MLX models"),
        None => said.to_string(),
    };
    format!("{app} {version} can't run this model: {why}.")
}

/// The model type in Ollama's `unsupported MLX architecture: model "X"`.
fn architecture(said: &str) -> Option<&str> {
    let rest = said.strip_prefix("unsupported MLX architecture")?;
    let start = rest.find('"')? + 1;
    let end = start + rest[start..].find('"')?;
    Some(&rest[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(version: &str, reason: &str) -> Refusal {
        Refusal {
            app: App::Ollama,
            version: version.into(),
            method: "clone-with-rewritten-index".into(),
            config: "c0".into(),
            reason: reason.into(),
            lasting: true,
            at: 1,
        }
    }

    #[test]
    fn keeps_one_answer_per_question() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        assert!(
            Refusals::load(&home)
                .find(App::Ollama, "0.40.16", "clone-with-rewritten-index", "c0")
                .is_none()
        );

        Refusals::record(&home, refusal("0.40.16", "first")).unwrap();
        Refusals::record(&home, refusal("0.40.16", "second")).unwrap();
        Refusals::record(&home, refusal("0.41.0", "other")).unwrap();
        let known = Refusals::load(&home);
        let method = "clone-with-rewritten-index";
        assert_eq!(
            known
                .find(App::Ollama, "0.40.16", method, "c0")
                .unwrap()
                .reason,
            "second"
        );
        assert_eq!(
            known
                .find(App::Ollama, "0.41.0", method, "c0")
                .unwrap()
                .reason,
            "other"
        );
        assert!(known.find(App::Ollama, "0.40.16", method, "c1").is_none());
        assert!(known.find(App::Bionic, "0.40.16", method, "c0").is_none());
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

    #[test]
    fn says_why_in_plain_words() {
        let kind = r#"unsupported MLX architecture: model "DeepseekV4ForCausalLM""#;
        let plain = "Ollama 0.40.16 can't run this model: it doesn't support the model type \
                     DeepseekV4ForCausalLM for MLX models.";
        assert_eq!(sentence("Ollama", "0.40.16", kind), plain);
        // As older builds kept it.
        assert_eq!(
            sentence("Ollama", "0.40.16", &format!("{OLDER_WORDS}{kind}")),
            plain
        );
        assert_eq!(
            sentence("Ollama", "0.40.16", "it doesn't run 5-bit MLX layers"),
            "Ollama 0.40.16 can't run this model: it doesn't run 5-bit MLX layers."
        );
    }

    #[test]
    fn only_an_answer_about_the_model_is_kept() {
        assert!(is_about_the_model(
            r#"unsupported MLX architecture: model "X""#
        ));
        assert!(is_about_the_model("Unknown data type: F8_E4M3"));
        assert!(!is_about_the_model("write /tmp/x: no space left on device"));
        assert!(!is_about_the_model("signal: killed"));
        assert!(!is_about_the_model("it gave no reason"));
        // The system's words, which only happen to say "not supported".
        assert!(!is_about_the_model(
            "clonefile: operation not supported (os error 45)"
        ));
        assert!(!is_about_the_model(
            "address family not supported by protocol"
        ));
        assert!(!is_about_the_model("unsupported value for OLLAMA_HOST"));
    }

    #[test]
    fn an_answer_kept_by_an_older_build_counts_only_if_it_is_about_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        let older = |config: &str, said: &str| Refusal {
            config: config.into(),
            reason: format!("{OLDER_WORDS}{said}"),
            lasting: false,
            ..refusal("0.40.16", "")
        };
        std::fs::write(
            path(&home),
            serde_json::to_vec(&[
                older("c0", r#"unsupported MLX architecture: model "X""#),
                older("c1", "signal: killed"),
            ])
            .unwrap(),
        )
        .unwrap();
        let known = Refusals::load(&home);
        let method = "clone-with-rewritten-index";
        assert!(known.find(App::Ollama, "0.40.16", method, "c0").is_some());
        assert!(known.find(App::Ollama, "0.40.16", method, "c1").is_none());
    }
}
