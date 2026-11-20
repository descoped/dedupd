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

    /// For a home on another disk, such as the archive: the disk is
    /// attached (the folder, or the folder it goes in, is there).
    pub fn attached(&self) -> bool {
        self.root.is_dir() || self.root.parent().is_some_and(Path::is_dir)
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

    /// The models marked as favorites, and the Hugging Face likes read.
    pub fn favorites_path(&self) -> PathBuf {
        self.root.join("favorites.json")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.store_dir().join("tmp")
    }

    /// Where the file with this sha256 lives: `store/sha256/ab/abcdef…`.
    ///
    /// Text that is not a sha256 names a file that never exists, inside the
    /// store's folder: nothing given here can point anywhere else.
    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        let folder = self.store_dir().join("sha256");
        if !crate::names::is_sha256(sha256) {
            return folder.join("xx").join("not-a-sha256");
        }
        folder.join(&sha256[..2]).join(sha256)
    }
}

#[cfg(test)]
mod tests {
    use super::Home;

    #[test]
    fn a_stored_files_path_cannot_leave_the_store() {
        let home = Home::new("/Users/example/.dedupd");
        let folder = home.store_dir().join("sha256");
        let sha = "ab".repeat(32);
        assert_eq!(home.blob_path(&sha), folder.join("ab").join(&sha));
        for bad in [
            "",
            "ab",
            "../../../../etc/hosts",
            "/Users/example/.ssh/id_ed25519",
            &sha.to_uppercase(),
            &format!("../{}", &sha[3..]),
        ] {
            let path = home.blob_path(bad);
            assert_eq!(path, folder.join("xx").join("not-a-sha256"), "{bad:?}");
        }
    }
}
