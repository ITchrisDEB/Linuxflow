//! Minimal system access: free disk space (`statvfs`).

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Free space (bytes) for an unprivileged user on the file system of `path`.
pub fn free_space(path: &Path) -> io::Result<u64> {
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `c` is a valid NUL-terminated C string; `st` points to a `statvfs` structure
    // of the right size that the call fills entirely on success (return 0).
    let rc = unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs succeeded, the structure is initialised.
    let st = unsafe { st.assume_init() };

    // Types vary by platform (u32/u64): deliberate conversions.
    #[allow(clippy::unnecessary_cast)]
    let free = (st.f_bavail as u64).saturating_mul(st.f_frsize as u64);
    Ok(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_of_tmp_dir_is_positive() {
        let d = tempfile::tempdir().unwrap();
        assert!(free_space(d.path()).unwrap() > 0);
    }

    #[test]
    fn missing_path_is_an_error() {
        assert!(free_space(Path::new("/nonexistent-dir-for-test/x")).is_err());
    }
}
