use std::path::{Path, PathBuf};

/// The directory that holds everything dedupd owns: the store and its database.
#[derive(Debug, Clone)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `~/.dedupd`, or `None` when `HOME` is not set.
    pub fn default_root() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".dedupd"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join("dedupd.sqlite")
    }

    pub fn store_dir(&self) -> PathBuf {
        self.root.join("store")
    }

    /// The settings the owner wrote.
    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// The daemon's log, which launchd writes.
    pub fn log_path(&self) -> PathBuf {
        self.root.join("daemon.log")
    }

    /// The copy of the command the launch agent starts.
    pub fn program_path(&self) -> PathBuf {
        self.root.join("bin").join("dedupd")
    }

    /// When the whole store was last checked, and what was found.
    pub fn last_verify_path(&self) -> PathBuf {
        self.root.join("last-verify.txt")
    }

    /// What happened to the models, one line each.
    pub fn activity_path(&self) -> PathBuf {
        self.root.join("activity.jsonl")
    }

    /// Held while an entry is added to the activity file.
    pub fn activity_lock_path(&self) -> PathBuf {
        self.root.join("activity.lock")
    }

    /// What an app said when it was asked about a model and said no.
    pub fn refusals_path(&self) -> PathBuf {
        self.root.join("refusals.json")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.store_dir().join("tmp")
    }

    /// Where the file with this sha256 lives: `store/sha256/ab/abcdef…`.
    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        let prefix = sha256.get(..2).unwrap_or("xx");
        self.store_dir().join("sha256").join(prefix).join(sha256)
    }
}
