//! APFS copy-on-write clones. A clone is an independent regular file that
//! shares disk blocks with its source until either one is modified.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Clone `src` to `dst`. Fails if `dst` exists or the two are on different
/// volumes. Never falls back to copying: a silent copy would double disk use.
#[cfg(target_os = "macos")]
pub fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_src = CString::new(src.as_os_str().as_bytes())?;
    let c_dst = CString::new(dst.as_os_str().as_bytes())?;
    // SAFETY: both pointers are valid NUL-terminated strings for the call.
    let rc = unsafe { libc::clonefile(c_src.as_ptr(), c_dst.as_ptr(), 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
pub fn clone_file(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "file clones are only implemented for APFS on macOS",
    ))
}

/// Put a clone of `src` at `dst`, replacing whatever is there in one step.
///
/// The clone is made under a hidden temporary name beside `dst` (what an
/// interrupted run left there is removed first), `prepare` then adjusts it
/// or refuses, and only then is it renamed onto `dst`. On any failure the
/// temporary file is removed and `dst` is as it was. The temporary name is
/// this call's own, so two callers placing one file do not take each
/// other's: both finish, with the same content in place.
pub fn clone_into_place(
    src: &Path,
    dst: &Path,
    prepare: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    clone_then(src, dst, prepare, |tmp, dst| std::fs::rename(tmp, dst))
}

/// Put a clone of `src` at `dst` only if nothing is there. A file that
/// appeared at `dst` in the meantime (an app finishing its own download) is
/// left as it is, and the error is `io::ErrorKind::AlreadyExists`.
pub fn clone_into_new_place(
    src: &Path,
    dst: &Path,
    prepare: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    clone_then(src, dst, prepare, rename_new)
}

/// Whether placing failed because something was already at the destination.
pub fn already_there(e: &anyhow::Error) -> bool {
    e.downcast_ref::<io::Error>()
        .is_some_and(|e| e.kind() == io::ErrorKind::AlreadyExists)
}

fn clone_then(
    src: &Path,
    dst: &Path,
    prepare: impl FnOnce(&Path) -> anyhow::Result<()>,
    finish: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> anyhow::Result<()> {
    use anyhow::Context;
    let parent = dst.parent().context("destination has no parent folder")?;
    let name = dst.file_name().context("destination has no file name")?;
    clear_leftovers_beside(dst);
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let tmp = parent.join(format!(
        ".{}.{}-{}{TMP_END}",
        name.to_string_lossy(),
        std::process::id(),
        CALLS.fetch_add(1, Ordering::Relaxed)
    ));
    clone_file(src, &tmp)?;
    let done = || -> anyhow::Result<()> {
        prepare(&tmp)?;
        finish(&tmp, dst)?;
        Ok(())
    };
    if let Err(e) = done() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// How the temporary clone beside a destination ends.
const TMP_END: &str = ".dedupd-tmp";

/// Remove the temporary clones a process that stopped part of the way left
/// beside `dst`: a clone shares its blocks with the stored file, so one
/// left behind keeps that space in use after the model is gone from the
/// app and the store. One whose process still runs is somebody's work and
/// stays. Called before a file is placed and when one is taken out.
pub fn clear_leftovers_beside(dst: &Path) {
    let (Some(parent), Some(name)) = (dst.parent(), dst.file_name()) else {
        return;
    };
    let start = format!(".{}.", name.to_string_lossy());
    // The name builds before this one gave it.
    let _ = std::fs::remove_file(parent.join(format!(".{}{TMP_END}", name.to_string_lossy())));
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let found = entry.file_name();
        let found = found.to_string_lossy();
        let pid = found
            .strip_prefix(&start)
            .and_then(|rest| rest.strip_suffix(TMP_END))
            .and_then(|made_by| made_by.split_once('-'))
            .and_then(|(pid, call)| {
                call.bytes()
                    .all(|b| b.is_ascii_digit())
                    .then(|| pid.parse::<u32>().ok())?
            });
        if pid.is_some_and(|pid| !crate::paths::process_is_alive(pid)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Rename `from` to `to`, failing when `to` exists: the check and the rename
/// are one step, so nothing can appear in between and be replaced.
#[cfg(target_os = "macos")]
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both are valid NUL-terminated paths that live for the call.
    let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| {
        std::ffi::CString::new(p.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
    };
    let (from, to) = (c(from)?, c(to)?);
    // SAFETY: both paths are valid NUL-terminated strings for the call.
    // The system call is used directly: not every C library has the wrapper.
    let done = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if done == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn rename_new(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this system has no rename that keeps an existing file",
    ))
}

/// True when both paths are on the same volume, which clones require.
pub fn same_volume(a: &Path, b: &Path) -> io::Result<bool> {
    Ok(std::fs::metadata(a)?.dev() == std::fs::metadata(b)?.dev())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn two_callers_placing_one_file_both_finish() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("model.gguf");
        std::fs::write(&src, vec![7u8; 64 * 1024]).unwrap();
        std::thread::scope(|scope| {
            let placing: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        for _ in 0..20 {
                            clone_into_place(&src, &dst, |_| Ok(())).unwrap();
                        }
                    })
                })
                .collect();
            for thread in placing {
                thread.join().unwrap();
            }
        });
        assert_eq!(std::fs::read(&dst).unwrap(), vec![7u8; 64 * 1024]);
        // And nothing but the two files is left in the folder.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn what_a_stopped_placement_left_is_cleared_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("model.gguf");
        let at = |name: &str| dir.path().join(name);
        let mine = format!(".model.gguf.{}-3.dedupd-tmp", std::process::id());
        for name in [
            "model.gguf",
            // Left by processes that are gone, and by a build before this one.
            ".model.gguf.4000000001-0.dedupd-tmp",
            ".model.gguf.dedupd-tmp",
            // Somebody's work now, another file's, and not a name of ours.
            mine.as_str(),
            ".other.gguf.4000000001-0.dedupd-tmp",
            ".model.gguf.notes",
        ] {
            std::fs::write(at(name), b"x").unwrap();
        }
        clear_leftovers_beside(&dst);
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut expected = vec![
            ".model.gguf.notes".to_string(),
            ".other.gguf.4000000001-0.dedupd-tmp".to_string(),
            mine,
            "model.gguf".to_string(),
        ];
        expected.sort();
        assert_eq!(left, expected);
    }

    #[test]
    fn clone_is_an_independent_file_with_the_same_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::write(&src, b"model bytes").unwrap();

        clone_file(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"model bytes");
        assert_ne!(
            std::fs::metadata(&src).unwrap().ino(),
            std::fs::metadata(&dst).unwrap().ino()
        );

        // Changing the source afterwards must not change the clone.
        std::fs::write(&src, b"changed").unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"model bytes");
    }

    #[test]
    fn a_file_that_appears_while_placing_is_left_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("model.gguf");
        std::fs::write(&src, b"from the store").unwrap();

        // Nothing there: the clone is placed.
        clone_into_new_place(&src, &dst, |_| Ok(())).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"from the store");
        std::fs::remove_file(&dst).unwrap();

        // The app writes its own file after dedupd looked and before it
        // finished: that file stays, and nothing of dedupd's is left.
        let appeared = clone_into_new_place(&src, &dst, |_| {
            std::fs::write(&dst, b"the app's own").unwrap();
            Ok(())
        })
        .unwrap_err();
        assert!(already_there(&appeared), "{appeared:#}");
        assert_eq!(std::fs::read(&dst).unwrap(), b"the app's own");
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
    }

    #[test]
    fn clone_into_place_replaces_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        let tmp = dir.path().join(".dst.dedupd-tmp");
        std::fs::write(&src, b"new").unwrap();
        std::fs::write(&dst, b"old").unwrap();
        std::fs::write(&tmp, b"stale").unwrap();

        // A refusal leaves the destination alone and no temporary file.
        let refused = clone_into_place(&src, &dst, |t| {
            assert_eq!(std::fs::read(t).unwrap(), b"new");
            anyhow::bail!("no")
        });
        assert!(refused.is_err());
        assert_eq!(std::fs::read(&dst).unwrap(), b"old");
        assert!(!tmp.exists());

        clone_into_place(&src, &dst, |_| Ok(())).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"new");
        assert!(!tmp.exists());
    }

    #[test]
    fn refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::write(&src, b"a").unwrap();
        std::fs::write(&dst, b"b").unwrap();
        assert!(clone_file(&src, &dst).is_err());
        assert_eq!(std::fs::read(&dst).unwrap(), b"b");
    }
}
