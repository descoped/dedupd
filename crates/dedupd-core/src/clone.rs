//! APFS copy-on-write clones. A clone is an independent regular file that
//! shares disk blocks with its source until either one is modified.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

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
/// The clone is made under a hidden temporary name beside `dst` (a leftover
/// from an interrupted run is removed first), `prepare` then adjusts it or
/// refuses, and only then is it renamed onto `dst`. On any failure the
/// temporary file is removed and `dst` is as it was.
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
    let tmp = parent.join(format!(".{}.dedupd-tmp", name.to_string_lossy()));
    let _ = std::fs::remove_file(&tmp);
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
