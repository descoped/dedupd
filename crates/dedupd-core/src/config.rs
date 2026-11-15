//! Settings read from `<home>/config.toml`. Every field is optional.

use crate::paths::Home;
use anyhow::{Context, Result};
use dedupd_api::daemon::Setting;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const DEFAULT_LISTEN: &str = dedupd_api::daemon::DEFAULT_ADDRESS;

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

/// Days between full checks of the store unless `verify_days` says otherwise.
pub const DEFAULT_VERIFY_DAYS: u64 = 7;
/// Seconds between scans of the app folders unless `scan_interval` says
/// otherwise.
pub const DEFAULT_SCAN_INTERVAL: u64 = 300;

impl Config {
    pub fn path(home: &Home) -> PathBuf {
        home.config_path()
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
    /// a setting to its new value; `null` removes the setting. The rest of
    /// the file, comments included, stays as the owner wrote it. Nothing is
    /// written when the result would not be a valid config.
    pub fn update(home: &Home, changes: &BTreeMap<Setting, serde_json::Value>) -> Result<Self> {
        let path = Self::path(home);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut doc: toml_edit::DocumentMut = text
            .parse()
            .with_context(|| format!("reading {}", path.display()))?;
        for (setting, value) in changes {
            let key = setting.as_str();
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
                Some(item) => doc[key] = item,
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

    pub fn hf_endpoint(&self) -> &str {
        self.hf_endpoint
            .as_deref()
            .unwrap_or(crate::hub::DEFAULT_ENDPOINT)
    }

    /// Duplicate app files are replaced with clones unless turned off.
    pub fn reclaim(&self) -> bool {
        self.reclaim != Some(false)
    }

    /// `HF_ENDPOINT` is set for the login session only when asked for.
    pub fn set_env(&self) -> bool {
        self.set_env == Some(true)
    }

    /// Days between full checks of the store; 0 turns them off.
    pub fn verify_days(&self) -> u64 {
        self.verify_days.unwrap_or(DEFAULT_VERIFY_DAYS)
    }

    /// Seconds between scans of the app folders when nothing was seen to
    /// change.
    pub fn scan_interval(&self) -> u64 {
        self.scan_interval.unwrap_or(DEFAULT_SCAN_INTERVAL)
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
        let changes = BTreeMap::from([
            (Setting::MaxConnections, serde_json::json!(4)),
            (Setting::VerifyDays, serde_json::Value::Null),
        ]);
        let config = Config::update(&home, &changes).unwrap();
        assert_eq!(
            (config.max_connections, config.verify_days),
            (Some(4), None)
        );
        assert_eq!(
            std::fs::read_to_string(Config::path(&home)).unwrap(),
            "# where the second copy goes\narchive = \"/Volumes/Archive/dedupd\"\nmax_connections = 4\n"
        );

        // A value of the wrong kind changes nothing.
        let bad = BTreeMap::from([(Setting::MaxConnections, serde_json::json!("many"))]);
        assert!(Config::update(&home, &bad).is_err());
        assert_eq!(Config::load(&home).unwrap().max_connections, Some(4));
    }

    /// What `dedupd install --env` does: a `set_env = false` line, or a
    /// comment that names the setting, does not keep it off.
    #[test]
    fn update_turns_set_env_on_over_a_false_line_and_a_comment() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        let changes = BTreeMap::from([(Setting::SetEnv, serde_json::json!(true))]);
        for text in [
            "# set_env = true\nset_env = false\n",
            "# set_env = true\n",
            "",
        ] {
            std::fs::write(Config::path(&home), text).unwrap();
            let config = Config::update(&home, &changes).unwrap();
            assert_eq!(config.set_env, Some(true), "from {text:?}");
            let written = std::fs::read_to_string(Config::path(&home)).unwrap();
            assert_eq!(
                written.matches("set_env = true").count(),
                if text.is_empty() { 1 } else { 2 }
            );
        }
    }
}
