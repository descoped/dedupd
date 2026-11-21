use crate::clone::{clone_file, same_volume};
use crate::paths::{Archive, Home, PRIMARY_STORE, StoreRoot};
use anyhow::{Context, Result, bail};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The content-addressed folder: a view of one store (`root`), the one it
/// writes to, with every other store and archive in sight. Files are named
/// by their sha256 and are read-only once inside. A view is cheap to clone;
/// `for_folder` and `named` give the view of another store.
#[derive(Clone)]
pub struct Store {
    root: StoreRoot,
    shared: std::sync::Arc<Shared>,
}

/// What every view of the store shares.
struct Shared {
    /// dedupd's own home: the database, the activity list, the refusals.
    home: Home,
    /// The stores (warm locations), the primary store first. Changed while
    /// the daemon runs, when the owner adds or removes one.
    stores: std::sync::RwLock<Vec<StoreRoot>>,
    /// The archives (cold locations), the backup archive first. Files
    /// missing here are brought back from any of them on demand. Changed
    /// while the daemon runs, when the owner adds or removes one.
    archives: std::sync::RwLock<Vec<Archive>>,
    /// One restore from the archive at a time, on purpose: the archive is
    /// one external disk, and two copies read from it at once are slower
    /// than one after the other. A request for a small file therefore
    /// waits for a large one that is being copied back. Files that are in
    /// the store never wait here. A copy from one store to another waits
    /// here too.
    restoring: std::sync::Mutex<()>,
    /// One copy to the archive at a time, for the same reason: the
    /// daemon's backup and an archive the owner asked for may also want
    /// the same file. Another process beside this one is kept apart by
    /// the temporary name, which carries the process id.
    copying: std::sync::Mutex<()>,
}

/// No store is on the volume of an app's folder, so nothing can be cloned
/// into it.
#[derive(Debug, thiserror::Error)]
#[error(
    "no store is on the disk of {0}: a model can be activated only in an app folder on the disk of a store; \
     add a store on that disk on the Storage screen"
)]
pub struct NoStoreThere(pub PathBuf);

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
        let primary = StoreRoot {
            name: PRIMARY_STORE.to_string(),
            home: home.clone(),
            primary: true,
        };
        Ok(Self {
            root: primary.clone(),
            shared: std::sync::Arc::new(Shared {
                home: home.clone(),
                stores: std::sync::RwLock::new(vec![primary]),
                archives: std::sync::RwLock::new(Vec::new()),
                restoring: std::sync::Mutex::new(()),
                copying: std::sync::Mutex::new(()),
            }),
        })
    }

    /// Use these stores besides the primary one, which stays first. Their
    /// folders are made when their disks are attached.
    pub fn set_stores(&self, others: Vec<StoreRoot>) {
        let primary = self.primary_root();
        let mut all = vec![primary];
        for other in others.into_iter().filter(|s| !s.primary) {
            if other.home.attached() {
                let _ = fs::create_dir_all(other.home.store_dir().join("sha256"));
                let _ = fs::create_dir_all(other.home.tmp_dir());
            }
            all.push(other);
        }
        *self
            .shared
            .stores
            .write()
            .unwrap_or_else(|e| e.into_inner()) = all;
    }

    /// Every store, the primary store first; a disk may be detached.
    pub fn stores(&self) -> Vec<StoreRoot> {
        self.shared
            .stores
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The stores whose disks are attached, the primary store first.
    pub fn attached_stores(&self) -> Vec<StoreRoot> {
        self.stores()
            .into_iter()
            .filter(|s| s.primary || s.home.store_dir().is_dir())
            .collect()
    }

    fn primary_root(&self) -> StoreRoot {
        StoreRoot {
            name: PRIMARY_STORE.to_string(),
            home: self.shared.home.clone(),
            primary: true,
        }
    }

    /// The view of the primary store.
    pub fn primary(&self) -> Store {
        self.view(self.primary_root())
    }

    fn view(&self, root: StoreRoot) -> Store {
        Store {
            root,
            shared: self.shared.clone(),
        }
    }

    /// The store this view writes to.
    pub fn root(&self) -> &StoreRoot {
        &self.root
    }

    /// The view of the store on the volume of `dir` (an app's folder, or a
    /// file in it), which may not be there yet: its nearest folder that is
    /// there decides. Fails with [`NoStoreThere`] when no attached store is
    /// on that volume.
    pub fn for_folder(&self, dir: &Path) -> Result<Store> {
        // A folder on a disk that is away has no volume: it is not taken
        // for the root volume, which `/Volumes` is on.
        let Ok(volume) = crate::paths::volume_of(dir) else {
            return Err(NoStoreThere(dir.to_path_buf()).into());
        };
        for root in self.attached_stores() {
            if crate::paths::volume_of(&root.home.store_dir()).is_ok_and(|v| v == volume) {
                return Ok(self.view(root));
            }
        }
        Err(NoStoreThere(dir.to_path_buf()).into())
    }

    /// The view of the store with this name, when its disk is attached.
    pub fn named(&self, name: &str) -> Result<Store> {
        let all = self.stores();
        let Some(root) = all.iter().find(|s| s.name.eq_ignore_ascii_case(name)) else {
            let names: Vec<_> = all.iter().map(|s| s.name.as_str()).collect();
            bail!(
                "there is no store named {name}; the stores are {}",
                names.join(", ")
            );
        };
        if !root.primary && !root.home.store_dir().is_dir() {
            bail!(
                "the disk of the store {} is not attached ({} not found)",
                root.name,
                root.home.root().display()
            );
        }
        Ok(self.view(root.clone()))
    }

    /// Whether any attached store has the file.
    pub fn contains_anywhere(&self, sha256: &str) -> bool {
        self.find(sha256).is_some()
    }

    /// Where an attached store keeps the file: this one first.
    pub fn find(&self, sha256: &str) -> Option<PathBuf> {
        let here = self.blob_path(sha256);
        if here.is_file() {
            return Some(here);
        }
        self.attached_stores()
            .into_iter()
            .map(|s| s.home.blob_path(sha256))
            .find(|p| p.is_file())
    }

    /// Make sure some store has the file, bringing it back from an archive
    /// into this one when no store has it. Returns false when none has it.
    pub fn ensure_stored(&self, sha256: &str) -> Result<bool> {
        if self.contains_anywhere(sha256) {
            return Ok(true);
        }
        self.ensure_local(sha256)
    }

    /// Remove the file from every attached store. Returns the bytes freed.
    pub fn remove_everywhere(&self, sha256: &str) -> Result<u64> {
        let mut freed = 0;
        let archives = self.archives();
        for root in self.attached_stores() {
            // A second guard beside the config's rule: a store that is (or
            // holds, or is in) an archive's folder is never emptied here,
            // because its file may be the archive's only copy.
            let (r, held) = (root.home.root(), &archives);
            if held
                .iter()
                .any(|a| r.starts_with(a.home.root()) || a.home.root().starts_with(r))
            {
                continue;
            }
            let path = root.home.blob_path(sha256);
            if let Ok(meta) = fs::metadata(&path) {
                fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
                freed += meta.len();
            }
        }
        Ok(freed)
    }

    /// Use `archive` as the backup archive, named after its disk. It may be
    /// on a disk that is not always attached; it is only looked at when a
    /// file is needed.
    pub fn with_archive(self, archive: Option<Home>) -> Self {
        self.set_archives(
            archive
                .into_iter()
                .map(|home| Archive {
                    name: crate::paths::default_archive_name(home.root()),
                    home,
                    backup: true,
                })
                .collect(),
        );
        self
    }

    /// Use these archives from now on (the backup archive first, as
    /// `Config::archive_list` gives them).
    pub fn set_archives(&self, archives: Vec<Archive>) {
        *self
            .shared
            .archives
            .write()
            .unwrap_or_else(|e| e.into_inner()) = archives;
    }

    /// Every archive, the backup archive first; their disks may be detached.
    pub fn archives(&self) -> Vec<Archive> {
        self.shared
            .archives
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The archives whose disks are attached, the backup archive first.
    pub fn attached_archives(&self) -> Vec<Archive> {
        self.archives()
            .into_iter()
            .filter(|a| a.home.attached())
            .collect()
    }

    /// The turn to copy a file to the archive disk; held while one is
    /// written.
    pub(crate) fn copying_to_archive(&self) -> std::sync::MutexGuard<'_, ()> {
        self.shared
            .copying
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// dedupd's own home (the database, the activity list), whichever store
    /// this view writes to.
    pub fn home(&self) -> &Home {
        &self.shared.home
    }

    /// Whether an attached archive has the file. Asks for the list of
    /// archives on each call; a loop over many files takes
    /// `attached_archives` once and asks `in_any`.
    pub fn in_an_archive(&self, sha256: &str) -> bool {
        in_any(&self.attached_archives(), sha256)
    }

    /// The home of the backup archive, when one is configured; its disk
    /// may be detached.
    pub fn archive(&self) -> Option<Home> {
        self.archives()
            .into_iter()
            .find(|a| a.backup)
            .map(|a| a.home)
    }

    /// Bytes free on the volume the store is on.
    pub fn free_bytes(&self) -> Result<u64> {
        Ok(self.volume()?.1)
    }

    /// The size of the volume the store is on and the bytes free on it.
    pub fn volume(&self) -> Result<(u64, u64)> {
        use std::os::unix::ffi::OsStrExt;
        let dir = self.root.home.store_dir();
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
        self.root.home.tmp_dir().join(name)
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

    /// Make sure the file is in this store: copied from another store on
    /// this Mac, or else back from an attached archive (the backup archive
    /// first; a damaged copy is passed over for the next one). Returns false
    /// when none has it.
    pub fn ensure_local(&self, sha256: &str) -> Result<bool> {
        if self.contains(sha256) {
            return Ok(true);
        }
        let mut damaged = None;
        let others = self
            .attached_stores()
            .into_iter()
            .filter(|s| s.home.root() != self.root.home.root())
            .map(|s| s.home);
        let archives = self.attached_archives().into_iter().map(|a| a.home);
        for from in others.chain(archives) {
            let src = from.blob_path(sha256);
            if !src.is_file() {
                continue;
            }
            match self.copy_back(&src, sha256) {
                Ok(()) => return Ok(true),
                Err(e) => damaged = Some(e),
            }
        }
        match damaged {
            Some(e) => Err(e),
            None => Ok(false),
        }
    }

    /// Copy one file back from an archive, hashing it as it is written.
    fn copy_back(&self, src: &Path, sha256: &str) -> Result<()> {
        let _restoring = self
            .shared
            .restoring
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self.contains(sha256) {
            // Another caller restored it while this one waited.
            return Ok(());
        }
        let tmp = self.tmp_path(&format!("{sha256}.restore.{}", std::process::id()));
        let _ = fs::remove_file(&tmp);
        let result = (|| -> Result<()> {
            let copied = crate::hash::copy_hashing(src, &tmp)
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
        result
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
        let mut dirs: Vec<PathBuf> = self
            .attached_stores()
            .iter()
            .map(|s| s.home.tmp_dir())
            .collect();
        dirs.extend(self.attached_archives().iter().map(|a| a.home.tmp_dir()));
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
        let root = self.root.home.store_dir().join("sha256");
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
        self.root.home.blob_path(sha256)
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

        if !same_volume(src, &self.root.home.store_dir())? {
            bail!(
                "{} is on a different volume from the store at {}; clones need one APFS volume",
                src.display(),
                self.root.home.store_dir().display()
            );
        }

        let tmp = self
            .root
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

/// Whether one of `archives` has the file.
pub fn in_any(archives: &[Archive], sha256: &str) -> bool {
    archives.iter().any(|a| a.home.blob_path(sha256).is_file())
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
    fn a_folder_ollama_was_asked_in_goes_when_its_process_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().join("home"));
        let store = Store::open(&home).unwrap();
        let gone = home.tmp_dir().join("ollama-asked-4000000007-2");
        let mine = home
            .tmp_dir()
            .join(format!("ollama-asked-{}-2", std::process::id()));
        for folder in [&gone, &mine] {
            fs::create_dir_all(folder.join("models")).unwrap();
            fs::write(folder.join("models/x"), b"x").unwrap();
        }
        assert_eq!(store.clear_leftovers(), 1);
        assert!(!gone.exists() && mine.exists());
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
