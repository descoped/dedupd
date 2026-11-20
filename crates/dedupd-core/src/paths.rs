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
    /// attached (the folder, or the folder it goes in, is there). A home
    /// that is a disk's own mount point (`/Volumes/Archive`) is attached
    /// only when it is there itself: `/Volumes` always is.
    pub fn attached(&self) -> bool {
        let parent = self.root.parent();
        let is_mount_point = parent == Some(Path::new("/Volumes"));
        self.root.is_dir() || (!is_mount_point && parent.is_some_and(Path::is_dir))
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

/// Write `bytes` as the whole of `target`, in one step: to a temporary
/// name beside it (with this process's id, so two processes do not share
/// one), synced to the disk, then renamed. A reader sees the old file or
/// the new one, never part of one, also after a power loss.
pub fn write_in_one_step(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let name = target
        .file_name()
        .ok_or_else(|| std::io::Error::other("the file has no name"))?
        .to_string_lossy();
    let tmp = target.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Whether a process with this id is running (also one of another user).
pub(crate) fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 sends nothing; the call only asks whether the
    // process can be signalled.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mount_point_that_is_not_there_is_a_disk_that_is_detached() {
        assert!(!Home::new("/Volumes/no-such-disk-for-this-test").attached());
        // A folder on a disk: its disk is attached when the disk's folder is.
        let dir = tempfile::tempdir().unwrap();
        assert!(Home::new(dir.path().join("dedupd")).attached());
        assert!(!Home::new(dir.path().join("gone/dedupd")).attached());
    }

    #[test]
    fn a_file_written_in_one_step_leaves_no_temporary_name() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("favorites.json");
        write_in_one_step(&target, b"one").unwrap();
        write_in_one_step(&target, b"two").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(process_is_alive(std::process::id()));
        assert!(!process_is_alive(4_000_000_000));
    }

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
