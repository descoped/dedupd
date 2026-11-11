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
    /// One restore from the archive at a time: parallel requests for the same
    /// file would otherwise write, and move, each other's temp file.
    restoring: std::sync::Mutex<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Added {
    /// A clone of the source file was added to the store.
    Cloned,
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
    pub fn of(path: &Path) -> std::io::Result<Self> {
        let m = fs::metadata(path)?;
        Ok(Self {
            size: m.len(),
            mtime_ns: m.mtime() * 1_000_000_000 + m.mtime_nsec(),
        })
    }
}

impl Store {
    pub fn open(home: &Home) -> Result<Self> {
        fs::create_dir_all(home.store_dir().join("sha256"))
            .with_context(|| format!("creating {}", home.store_dir().display()))?;
        fs::create_dir_all(home.tmp_dir())?;
        Ok(Self {
            home: home.clone(),
            archive: None,
            restoring: std::sync::Mutex::new(()),
        })
    }

    /// Use `archive` as the second copy. It may be on a disk that is not
    /// always attached; it is only looked at when a file is needed.
    pub fn with_archive(mut self, archive: Option<Home>) -> Self {
        self.archive = archive;
        self
    }

    pub fn home(&self) -> &Home {
        &self.home
    }

    pub fn archive(&self) -> Option<&Home> {
        self.archive.as_ref()
    }

    /// Bytes free on the volume the store is on.
    pub fn free_bytes(&self) -> Result<u64> {
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
        Ok(stat.f_bavail as u64 * stat.f_frsize as u64)
    }

    /// A path in the store's own temp folder, on the store's volume.
    pub fn tmp_path(&self, name: &str) -> PathBuf {
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
        Ok(Added::Cloned)
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
        let copied = crate::hash::copy_hashing(&src, &tmp)
            .with_context(|| format!("copying {} back from the archive", src.display()))?;
        if copied != sha256 {
            let _ = fs::remove_file(&tmp);
            bail!(
                "archive file {} is damaged: it hashes to {copied}",
                src.display()
            );
        }
        self.add_tmp(&tmp, sha256)?;
        Ok(true)
    }

    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        self.home.blob_path(sha256)
    }

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
        Ok(Added::Cloned)
    }
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
            Added::Cloned
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
