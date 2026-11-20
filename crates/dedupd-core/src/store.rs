use crate::clone::{clone_file, same_volume};
use crate::paths::Home;
use anyhow::{Context, Result, bail};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The content-addressed folder. Files are named by their sha256 and are
/// read-only once inside.
pub struct Store {
    home: Home,
    /// A second copy of the store on another disk. Files missing here are
    /// brought back from it on demand.
    archive: Option<Home>,
    /// One restore from the archive at a time, on purpose: the archive is
    /// one external disk, and two copies read from it at once are slower
    /// than one after the other. A request for a small file therefore
    /// waits for a large one that is being copied back. Files that are in
    /// the store never wait here.
    restoring: std::sync::Mutex<()>,
    /// One copy to the archive at a time, for the same reason: the
    /// daemon's backup and an archive the owner asked for may also want
    /// the same file. Another process beside this one is kept apart by
    /// the temporary name, which carries the process id.
    copying: std::sync::Mutex<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Added {
    /// The file was added to the store: a clone of the source file, or a
    /// finished temp file moved in.
    Stored,
    /// The store already held this content; nothing was written.
    AlreadyPresent,
}

/// What a source file looked like when it was hashed. `add_clone` checks the
/// file still looks like this after cloning, so a file that was being written
/// during the hash cannot enter the store under the wrong name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    pub size: u64,
    pub mtime_ns: i64,
}

impl Fingerprint {
    /// The file's size and time, or `None` when the file is no longer
    /// there: an app renames a download that finished and removes a model
    /// while its folder is being read, and that is not a failure.
    pub fn if_there(path: &Path) -> std::io::Result<Option<Self>> {
        match Self::of(path) {
            Ok(found) => Ok(Some(found)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The file's size and time as they are now.
    pub fn of(path: &Path) -> std::io::Result<Self> {
        let m = fs::metadata(path)?;
        Ok(Self {
            size: m.len(),
            mtime_ns: m.mtime() * 1_000_000_000 + m.mtime_nsec(),
        })
    }
}

impl Store {
    /// The store in `home`, with its folders made. No archive disk yet;
    /// see `with_archive`.
    pub fn open(home: &Home) -> Result<Self> {
        fs::create_dir_all(home.store_dir().join("sha256"))
            .with_context(|| format!("creating {}", home.store_dir().display()))?;
        fs::create_dir_all(home.tmp_dir())
            .with_context(|| format!("creating {}", home.tmp_dir().display()))?;
        Ok(Self {
            home: home.clone(),
            archive: None,
            restoring: std::sync::Mutex::new(()),
            copying: std::sync::Mutex::new(()),
        })
    }

    /// Use `archive` as the second copy. It may be on a disk that is not
    /// always attached; it is only looked at when a file is needed.
    pub fn with_archive(mut self, archive: Option<Home>) -> Self {
        self.archive = archive;
        self
    }

    /// The turn to copy a file to the archive disk; held while one is
    /// written.
    pub(crate) fn copying_to_archive(&self) -> std::sync::MutexGuard<'_, ()> {
        self.copying.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The home this store is in.
    pub fn home(&self) -> &Home {
        &self.home
    }

    /// The home on the archive disk, when one is configured; its disk
    /// may be detached.
    pub fn archive(&self) -> Option<&Home> {
        self.archive.as_ref()
    }

    /// Bytes free on the volume the store is on.
    pub fn free_bytes(&self) -> Result<u64> {
        Ok(self.volume()?.1)
    }

    /// The size of the volume the store is on and the bytes free on it.
    pub fn volume(&self) -> Result<(u64, u64)> {
        use std::os::unix::ffi::OsStrExt;
        let dir = self.home.store_dir();
        let path = std::ffi::CString::new(dir.as_os_str().as_bytes())?;
        // SAFETY: `statvfs` is plain data, and `path` is a valid C string
        // that outlives the call.
        let stat = unsafe {
            let mut stat: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(path.as_ptr(), &mut stat) != 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("asking for the free space of {}", dir.display()));
            }
            stat
        };
        let block = stat.f_frsize as u64;
        Ok((stat.f_blocks as u64 * block, stat.f_bavail as u64 * block))
    }

    /// A path in the store's own temp folder, on the store's volume.
    /// Only the last part of `name` is used, so the path stays in that
    /// folder whatever the name holds.
    pub fn tmp_path(&self, name: &str) -> PathBuf {
        let name = Path::new(name)
            .file_name()
            .unwrap_or(std::ffi::OsStr::new("unnamed"));
        self.home.tmp_dir().join(name)
    }

    /// Move a finished temp file into the store. The caller has verified that
    /// its content hashes to `sha256`.
    pub fn add_tmp(&self, tmp: &Path, sha256: &str) -> Result<Added> {
        let dst = self.blob_path(sha256);
        if dst.is_file() {
            let _ = fs::remove_file(tmp);
            return Ok(Added::AlreadyPresent);
        }
        fs::set_permissions(tmp, fs::Permissions::from_mode(0o444))?;
        fs::create_dir_all(dst.parent().expect("blob path has a parent"))?;
        fs::rename(tmp, &dst)
            .with_context(|| format!("moving {} into the store", tmp.display()))?;
        Ok(Added::Stored)
    }

    /// Make sure the file is in the local store, copying it back from the
    /// archive disk if that is the only place it is. Returns false when
    /// neither has it.
    pub fn ensure_local(&self, sha256: &str) -> Result<bool> {
        if self.contains(sha256) {
            return Ok(true);
        }
        let Some(archive) = &self.archive else {
            return Ok(false);
        };
        let src = archive.blob_path(sha256);
        if !src.is_file() {
            return Ok(false);
        }
        let _restoring = self.restoring.lock().unwrap_or_else(|e| e.into_inner());
        if self.contains(sha256) {
            // Another caller restored it while this one waited.
            return Ok(true);
        }
        let tmp = self.tmp_path(&format!("{sha256}.restore.{}", std::process::id()));
        let _ = fs::remove_file(&tmp);
        let result = (|| -> Result<()> {
            let copied = crate::hash::copy_hashing(&src, &tmp)
                .with_context(|| format!("copying {} back from the archive", src.display()))?;
            if copied != sha256 {
                bail!(
                    "archive file {} is damaged: it hashes to {copied}",
                    src.display()
                );
            }
            self.add_tmp(&tmp, sha256)?;
            Ok(())
        })();
        // A copy cut short (the disk went, the internal disk is full)
        // leaves nothing behind.
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result.map(|()| true)
    }

    /// Remove the half-made copies a process that stopped part of the way
    /// left in the temp folders, here and on the archive disk: a file
    /// cloned or copied into the store, copied back from the archive, or
    /// copied to it, and a copy of the database on its way there. A copy that is still being written was written to a
    /// moment ago and is left alone, and so is one whose process still
    /// runs (a clone carries its source's time, so its age says nothing);
    /// partial downloads, which are continued, are never touched. Returns
    /// how many were removed.
    pub fn clear_leftovers(&self) -> usize {
        let quiet_for = std::time::Duration::from_secs(600);
        let mut dirs = vec![self.home.tmp_dir()];
        dirs.extend(
            self.archive
                .as_ref()
                .filter(|a| a.attached())
                .map(Home::tmp_dir),
        );
        let mut removed = 0;
        for entry in dirs
            .iter()
            .filter_map(|d| fs::read_dir(d).ok())
            .flatten()
            .flatten()
        {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // A folder Ollama was asked in, whose process stopped before
            // it could remove it.
            if let Some(pid) = asked_folder_of(&name) {
                if !crate::paths::process_is_alive(pid) && fs::remove_dir_all(entry.path()).is_ok()
                {
                    removed += 1;
                }
                continue;
            }
            let Some(pid) = leftover_copy_of(&name) else {
                continue;
            };
            if crate::paths::process_is_alive(pid) {
                continue;
            }
            let idle = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age >= quiet_for);
            if idle && fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// The sha256 of every file in the store, read from the names in its
    /// folder (`sha256/ab/<hex>`). Anything else there is left out: a name
    /// that is not 64 lowercase hex digits, or one in the wrong subfolder.
    /// A folder that cannot be read is left out too, without an error: the
    /// caller only adds records for what is listed. Do not use the list to
    /// decide what to remove.
    pub fn stored_hashes(&self) -> Vec<String> {
        let root = self.home.store_dir().join("sha256");
        let mut out = Vec::new();
        for prefix in fs::read_dir(&root).into_iter().flatten().flatten() {
            let prefix_name = prefix.file_name().to_string_lossy().into_owned();
            for entry in fs::read_dir(prefix.path()).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_file = entry.file_type().is_ok_and(|t| t.is_file());
                if is_file && crate::names::is_sha256(&name) && name.starts_with(&prefix_name) {
                    out.push(name);
                }
            }
        }
        out.sort();
        out
    }

    /// Where the store keeps the file with this sha256, there or not.
    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        self.home.blob_path(sha256)
    }

    /// Whether the file is in the store on this disk (the archive disk
    /// is not asked).
    pub fn contains(&self, sha256: &str) -> bool {
        self.blob_path(sha256).is_file()
    }

    /// Add `src` to the store under `sha256` by cloning it. The source file is
    /// only read, never changed.
    pub fn add_clone(&self, src: &Path, sha256: &str, seen: Fingerprint) -> Result<Added> {
        let dst = self.blob_path(sha256);
        if let Ok(existing) = fs::metadata(&dst) {
            if existing.len() != seen.size {
                bail!(
                    "store file {} is {} bytes but {} hashes to the same id at {} bytes",
                    dst.display(),
                    existing.len(),
                    src.display(),
                    seen.size
                );
            }
            return Ok(Added::AlreadyPresent);
        }

        if !same_volume(src, &self.home.store_dir())? {
            bail!(
                "{} is on a different volume from the store at {}; clones need one APFS volume",
                src.display(),
                self.home.store_dir().display()
            );
        }

        let tmp = self
            .home
            .tmp_dir()
            .join(format!("{sha256}.{}", std::process::id()));
        let _ = fs::remove_file(&tmp);
        clone_file(src, &tmp)
            .with_context(|| format!("cloning {} into the store", src.display()))?;

        let finish = || -> Result<()> {
            if Fingerprint::of(src)? != seen {
                bail!("{} changed while it was being adopted", src.display());
            }
            if fs::metadata(&tmp)?.len() != seen.size {
                bail!("clone of {} has the wrong size", src.display());
            }
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o444))?;
            fs::create_dir_all(dst.parent().expect("blob path has a parent"))?;
            fs::rename(&tmp, &dst)?;
            Ok(())
        };
        if let Err(e) = finish() {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(Added::Stored)
    }
}

/// A temp file named as a copy on its way into the store or the archive:
/// `<sha256>.<pid>` or `<sha256>.restore.<pid>`.
#[cfg(test)]
fn is_leftover_copy(name: &str) -> bool {
    leftover_copy_of(name).is_some()
}

/// The name of a copy of the database on its way to the archive disk,
/// made by the process `pid`: in the temp folder here, and in the
/// archive's.
pub(crate) fn db_copy_name(pid: u32) -> String {
    format!("dedupd.sqlite.{pid}.backup")
}

/// How a folder Ollama is asked about a model in is named: this, the id
/// of the process and a time (`ollama_mlx`).
pub(crate) const ASKED_FOLDER: &str = "ollama-asked-";

/// The id of the process that made such a folder, when `name` is one.
fn asked_folder_of(name: &str) -> Option<u32> {
    let (pid, time) = name.strip_prefix(ASKED_FOLDER)?.split_once('-')?;
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    // A number too large for a process id is nobody's.
    (digits(pid) && digits(time)).then(|| pid.parse().unwrap_or(0))
}

/// The id of the process that made a half-made copy, when `name` is one.
fn leftover_copy_of(name: &str) -> Option<u32> {
    if let Some(pid) = name
        .strip_prefix("dedupd.sqlite.")
        .and_then(|rest| rest.strip_suffix(".backup"))
    {
        return (!pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
            .then(|| pid.parse().unwrap_or(0));
    }
    let (sha, rest) = name.split_once('.')?;
    let pid = rest.strip_prefix("restore.").unwrap_or(rest);
    let named_so = sha.len() == 64
        && sha.bytes().all(|b| b.is_ascii_hexdigit())
        && !pid.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit());
    // A number too large for a process id is nobody's.
    named_so.then(|| pid.parse().unwrap_or(0))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::hash::hash_file;

    #[test]
    fn add_clone_is_idempotent_and_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().join("home"));
        let store = Store::open(&home).unwrap();
        let src = dir.path().join("weights.gguf");
        fs::write(&src, b"weights").unwrap();
        let h = hash_file(&src, false).unwrap();
        let seen = Fingerprint::of(&src).unwrap();

        assert_eq!(
            store.add_clone(&src, &h.sha256, seen).unwrap(),
            Added::Stored
        );
        assert_eq!(
            store.add_clone(&src, &h.sha256, seen).unwrap(),
            Added::AlreadyPresent
        );

        let blob = store.blob_path(&h.sha256);
        assert_eq!(fs::read(&blob).unwrap(), b"weights");
        assert_eq!(
            fs::metadata(&blob).unwrap().permissions().mode() & 0o777,
            0o444
        );
        // The source keeps its own permissions and content.
        assert_ne!(fs::metadata(&src).unwrap().permissions().mode() & 0o222, 0);
        assert!(fs::read_dir(home.tmp_dir()).unwrap().next().is_none());
    }

    #[test]
    fn half_made_copies_are_recognised_and_partial_downloads_are_not() {
        let sha = "a".repeat(64);
        assert!(is_leftover_copy(&format!("{sha}.123")));
        assert!(is_leftover_copy(&format!("{sha}.restore.123")));
        assert!(is_leftover_copy(&db_copy_name(123)));
        assert!(!is_leftover_copy("dedupd.sqlite"));
        assert!(!is_leftover_copy(&format!("{sha}.part")));
        assert!(!is_leftover_copy(&format!("{sha}.asm")));
        assert!(!is_leftover_copy("ollama-asked-1-2"));
        assert_eq!(asked_folder_of("ollama-asked-12-345"), Some(12));
        assert_eq!(asked_folder_of("ollama-asked-12"), None);
        assert_eq!(asked_folder_of("ollama-asked-x-1"), None);
        assert!(!is_leftover_copy(&format!("{}.123", "a".repeat(63))));
    }

    #[test]
    fn a_left_over_copy_is_removed_once_it_has_been_idle() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().join("home"));
        let store = Store::open(&home).unwrap();
        let (old, new, part) = (
            // Ids no process has: the ones that made these are gone.
            home.tmp_dir()
                .join(format!("{}.restore.4000000007", "b".repeat(64))),
            home.tmp_dir()
                .join(format!("{}.4000000008", "c".repeat(64))),
            home.tmp_dir().join(format!("{}.part", "d".repeat(64))),
        );
        for path in [&old, &new, &part] {
            fs::write(path, b"x").unwrap();
        }
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for path in [&old, &part] {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        }
        assert_eq!(store.clear_leftovers(), 1);
        assert!(!old.exists() && new.exists() && part.exists());
    }

    #[test]
    fn a_copy_whose_process_still_runs_is_left_however_old_it_looks() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().join("home"));
        let store = Store::open(&home).unwrap();
        // A clone carries its source's time: it looks idle at once.
        let mine = home
            .tmp_dir()
            .join(format!("{}.{}", "e".repeat(64), std::process::id()));
        fs::write(&mine, b"x").unwrap();
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(&mine)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        assert_eq!(store.clear_leftovers(), 0);
        assert!(mine.exists());
    }

    #[test]
    fn a_damaged_copy_on_the_archive_disk_is_not_brought_back() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().join("home"));
        let archive = Home::new(dir.path().join("archive"));
        let store = Store::open(&home)
            .unwrap()
            .with_archive(Some(archive.clone()));
        let sha = "f".repeat(64);
        let copy = archive.blob_path(&sha);
        fs::create_dir_all(copy.parent().unwrap()).unwrap();
        fs::write(&copy, b"not what the name says").unwrap();

        let said = store.ensure_local(&sha).unwrap_err();
        assert!(format!("{said:#}").contains("is damaged"), "{said:#}");
        assert!(!store.contains(&sha));
        // Nothing half-made stays behind.
        assert_eq!(fs::read_dir(home.tmp_dir()).unwrap().count(), 0);
    }

    #[test]
    fn a_stored_file_of_another_size_under_the_same_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().join("home"));
        let store = Store::open(&home).unwrap();
        let src = dir.path().join("f");
        fs::write(&src, b"one").unwrap();
        let h = hash_file(&src, false).unwrap();
        let held = store.blob_path(&h.sha256);
        fs::create_dir_all(held.parent().unwrap()).unwrap();
        fs::write(&held, b"longer than one").unwrap();

        let said = store
            .add_clone(&src, &h.sha256, Fingerprint::of(&src).unwrap())
            .unwrap_err();
        assert!(said.to_string().contains("hashes to the same id"), "{said}");
        assert_eq!(fs::read(&held).unwrap(), b"longer than one");
    }

    #[test]
    fn rejects_a_file_that_changed_after_hashing() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().join("home"));
        let store = Store::open(&home).unwrap();
        let src = dir.path().join("f");
        fs::write(&src, b"one").unwrap();
        let h = hash_file(&src, false).unwrap();
        let stale = Fingerprint {
            size: 3,
            mtime_ns: 1,
        };

        assert!(store.add_clone(&src, &h.sha256, stale).is_err());
        assert!(!store.contains(&h.sha256));
        assert!(fs::read_dir(home.tmp_dir()).unwrap().next().is_none());
    }
}
