use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

const BUF_SIZE: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHashes {
    pub size: u64,
    pub sha256: String,
    /// The id git gives this content as a blob. This is what the Hub reports
    /// as `oid` for files that are not stored in LFS.
    pub git_oid: Option<String>,
}

/// Hash a file in one pass. `with_git_oid` also computes the git blob id,
/// which costs a second hash and is only useful for small, non-LFS files.
pub fn hash_file(path: &Path, with_git_oid: bool) -> io::Result<FileHashes> {
    hash_opened(File::open(path)?, path, with_git_oid)
}

/// Hash a file as the disk holds it: the bytes are read from the medium,
/// not from what the system still has in memory after writing them. For a
/// copy that must be known good before the other one is removed. It tells
/// the truth only for a file written by `copy_hashing`, which keeps its
/// bytes out of the system's memory too.
pub fn hash_file_from_disk(path: &Path) -> io::Result<FileHashes> {
    let file = File::open(path)?;
    uncached(&file);
    hash_opened(file, path, false)
}

/// Have reads and writes of `file` go past the system's cache.
#[cfg(target_os = "macos")]
fn uncached(file: &File) {
    use std::os::fd::AsRawFd;
    // SAFETY: the descriptor is open for as long as `file` is borrowed, and
    // `F_NOCACHE` takes an integer. A failure leaves the cache on, which
    // costs the check its strength on that disk and nothing else.
    unsafe {
        libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1);
    }
}

#[cfg(not(target_os = "macos"))]
fn uncached(_file: &File) {}

fn hash_opened(mut file: File, path: &Path, with_git_oid: bool) -> io::Result<FileHashes> {
    let expected = file.metadata()?.len();

    let mut sha256 = Sha256::new();
    let mut git = with_git_oid.then(|| {
        let mut h = Sha1::new();
        h.update(format!("blob {expected}\0").as_bytes());
        h
    });

    let mut buf = vec![0u8; BUF_SIZE];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha256.update(&buf[..n]);
        if let Some(h) = git.as_mut() {
            h.update(&buf[..n]);
        }
        size += n as u64;
    }
    if size != expected {
        return Err(io::Error::other(format!(
            "{} changed size while being read ({expected} -> {size})",
            path.display()
        )));
    }

    Ok(FileHashes {
        size,
        sha256: hex::encode(sha256.finalize()),
        git_oid: git.map(|h| hex::encode(h.finalize())),
    })
}

/// Copy `src` to `dst` (which must not exist), returning the sha256 of the
/// bytes written. Used between disks, where a clone is not possible. The
/// bytes written are not kept in the system's memory, so reading the copy
/// afterwards (`hash_file_from_disk`) asks the disk.
pub fn copy_hashing(src: &Path, dst: &Path) -> io::Result<String> {
    use std::io::{Read, Write};
    let mut input = std::fs::File::open(src)?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)?;
    uncached(&output);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        output.write_all(&buf[..n])?;
    }
    output.sync_all()?;
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"hello\n").unwrap();
        let h = hash_file(&p, true).unwrap();
        assert_eq!(h.size, 6);
        assert_eq!(
            h.sha256,
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(
            h.git_oid.as_deref(),
            Some("ce013625030ba8dba906f756967f9e9ca394464a")
        );
        assert_eq!(hash_file(&p, false).unwrap().git_oid, None);
    }

    #[test]
    fn empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("empty");
        std::fs::write(&p, b"").unwrap();
        let h = hash_file(&p, true).unwrap();
        assert_eq!(
            h.git_oid.as_deref(),
            Some("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")
        );
    }
}
