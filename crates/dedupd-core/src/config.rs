//! Settings read from `<home>/config.toml`. Every field is optional.

use crate::paths::Home;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

pub const DEFAULT_LISTEN: &str = "127.0.0.1:8642";

#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Folder on another disk that holds the second copy of the store, for
    /// example `/Volumes/Archive/dedupd`.
    pub archive: Option<PathBuf>,
    /// Address the daemon listens on.
    pub listen: Option<String>,
    /// Hugging Face endpoint to fetch from.
    pub hf_endpoint: Option<String>,
    /// App folders, when they are not in the default place.
    pub bionic_models: Option<PathBuf>,
    pub ollama_models: Option<PathBuf>,
    pub hf_cache: Option<PathBuf>,
    /// Whether the daemon replaces duplicate app files with clones.
    pub reclaim: Option<bool>,
    /// Whether the daemon points apps started from the Dock or Finder at
    /// itself by setting `HF_ENDPOINT` for the login session.
    pub set_env: Option<bool>,
    /// Days between the daemon's full checks of the store; 0 turns them off.
    pub verify_days: Option<u64>,
    /// Seconds between the daemon's scans of the app folders when nothing
    /// was seen to change.
    pub scan_interval: Option<u64>,
    /// The most downloads to run at once. How many actually run follows
    /// what the provider can take.
    pub max_connections: Option<usize>,
    /// Bytes per second all downloads together may use; 0 is no limit.
    pub speed_limit: Option<u64>,
}

impl Config {
    pub fn path(home: &Home) -> PathBuf {
        home.root().join("config.toml")
    }

    /// Read the config file. A missing file is an empty config.
    pub fn load(home: &Home) -> Result<Self> {
        let path = Self::path(home);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("reading {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Change settings in the config file and read it again. `changes` maps
    /// a setting's name to its new value; `null` removes the setting. The
    /// rest of the file, comments included, stays as the owner wrote it.
    /// Nothing is written when the result would not be a valid config.
    pub fn update(
        home: &Home,
        changes: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self> {
        let path = Self::path(home);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut doc: toml_edit::DocumentMut = text
            .parse()
            .with_context(|| format!("reading {}", path.display()))?;
        for (key, value) in changes {
            let item = match value {
                serde_json::Value::Null => None,
                serde_json::Value::Bool(b) => Some(toml_edit::value(*b)),
                serde_json::Value::String(s) => Some(toml_edit::value(s.as_str())),
                serde_json::Value::Number(n) => match n.as_i64() {
                    Some(i) => Some(toml_edit::value(i)),
                    None => anyhow::bail!("{key} must be a whole number"),
                },
                _ => anyhow::bail!("{key} cannot be a list or a table"),
            };
            match item {
                Some(item) => doc[key.as_str()] = item,
                None => {
                    doc.remove(key);
                }
            }
        }
        let text = doc.to_string();
        let config: Self = toml::from_str(&text).context("the settings are not valid")?;
        std::fs::create_dir_all(home.root())?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(config)
    }

    pub fn listen(&self) -> &str {
        self.listen.as_deref().unwrap_or(DEFAULT_LISTEN)
    }

    pub fn max_connections(&self) -> usize {
        self.max_connections
            .unwrap_or(crate::governor::DEFAULT_CONNECTIONS)
            .max(1)
    }

    pub fn speed_limit(&self) -> u64 {
        self.speed_limit.unwrap_or(0)
    }

    pub fn archive_home(&self) -> Option<Home> {
        self.archive.clone().map(Home::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        std::fs::write(
            Config::path(&home),
            "# where the second copy goes\narchive = \"/Volumes/Archive/dedupd\"\nverify_days = 30\n",
        )
        .unwrap();
        let changes = serde_json::json!({"max_connections": 4, "verify_days": null});
        let config = Config::update(&home, changes.as_object().unwrap()).unwrap();
        assert_eq!(
            (config.max_connections, config.verify_days),
            (Some(4), None)
        );
        assert_eq!(
            std::fs::read_to_string(Config::path(&home)).unwrap(),
            "# where the second copy goes\narchive = \"/Volumes/Archive/dedupd\"\nmax_connections = 4\n"
        );

        // A setting that does not exist, or a value of the wrong kind,
        // changes nothing.
        for bad in [
            serde_json::json!({"no_such_setting": 1}),
            serde_json::json!({"max_connections": "many"}),
        ] {
            assert!(Config::update(&home, bad.as_object().unwrap()).is_err());
        }
        assert_eq!(Config::load(&home).unwrap().max_connections, Some(4));
    }

    /// What `dedupd install --env` does: a `set_env = false` line, or a
    /// comment that names the setting, does not keep it off.
    #[test]
    fn update_turns_set_env_on_over_a_false_line_and_a_comment() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        let changes = serde_json::json!({"set_env": true});
        for text in [
            "# set_env = true\nset_env = false\n",
            "# set_env = true\n",
            "",
        ] {
            std::fs::write(Config::path(&home), text).unwrap();
            let config = Config::update(&home, changes.as_object().unwrap()).unwrap();
            assert_eq!(config.set_env, Some(true), "from {text:?}");
            let written = std::fs::read_to_string(Config::path(&home)).unwrap();
            assert_eq!(
                written.matches("set_env = true").count(),
                if text.is_empty() { 1 } else { 2 }
            );
        }
    }
}
