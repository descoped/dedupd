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
