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

    pub fn tmp_dir(&self) -> PathBuf {
        self.store_dir().join("tmp")
    }

    /// Where the file with this sha256 lives: `store/sha256/ab/abcdef…`.
    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        let prefix = sha256.get(..2).unwrap_or("xx");
        self.store_dir().join("sha256").join(prefix).join(sha256)
    }
}
