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
    /// example `/Volumes/Trunk/dedupd`.
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
