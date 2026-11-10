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
    use anyhow::Context;
    let parent = dst.parent().context("destination has no parent folder")?;
    let name = dst.file_name().context("destination has no file name")?;
    let tmp = parent.join(format!(".{}.dedupd-tmp", name.to_string_lossy()));
    let _ = std::fs::remove_file(&tmp);
    clone_file(src, &tmp)?;
    let finish = || -> anyhow::Result<()> {
        prepare(&tmp)?;
        std::fs::rename(&tmp, dst)?;
        Ok(())
    };
    if let Err(e) = finish() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
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
