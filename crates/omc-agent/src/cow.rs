//! Copy-on-write clones of folders, which isolated subagents work in when the file system can make them.
//!
//! A clone shares the storage of the folder it was made from until either side writes, so making one costs little whatever the folder holds, its dependencies and build output included. On macOS each entry of the folder is cloned with one `clonefile(2)` call, which clones a whole folder at once on APFS. On Linux the folder is walked and every file cloned with the `FICLONE` ioctl, which btrfs, XFS, bcachefs and OCFS2 answer; folders and symbolic links are made anew, and every file and folder keeps its permissions and modification time. Everywhere else, and on file systems that cannot share storage, cloning fails with an error [`unsupported`] recognises, and the caller does something else.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

/// The file that, in tests, has [`clone_entries`] refuse a folder holding it as a file system without clones would, so that what happens then can be tried on any host.
#[cfg(test)]
pub(crate) const REFUSE: &str = ".omc-refuse-clones";

/// Fills the folder `to`, made when it is missing, with clones of every entry of the folder `from` except those named in `skip`, which are matched against the names of `from`'s own entries only; sockets, pipes and devices are left out. Symbolic links are cloned as links, never followed.
///
/// # Errors
///
/// Fails with an error [`unsupported`] recognises when the file system, the platform, or the two folders being on different devices does not let files share storage, and with the failing call's error otherwise. What was cloned before a failure is left in `to` for the caller to remove.
pub(crate) fn clone_entries(from: &Path, to: &Path, skip: &[&OsStr]) -> io::Result<()> {
    #[cfg(test)]
    if from.join(REFUSE).exists() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this folder refuses clones",
        ));
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if skip.contains(&entry.file_name().as_os_str()) {
            continue;
        }
        let kind = entry.file_type()?;
        if !(kind.is_file() || kind.is_dir() || kind.is_symlink()) {
            continue;
        }
        imp::clone_entry(&entry.path(), &to.join(entry.file_name()), kind)?;
    }
    Ok(())
}

/// Whether `error`, from [`clone_entries`], means files cannot be cloned here at all, rather than that something went wrong that copying would run into too.
pub(crate) fn unsupported(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Unsupported
        || error.kind() == io::ErrorKind::CrossesDevices
        || error.raw_os_error().is_some_and(imp::unsupported_code)
}

#[cfg(target_os = "macos")]
/// Cloning with `clonefile(2)`.
mod imp {
    use std::ffi::CString;
    use std::fs::FileType;
    use std::io;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    /// The flag that has `clonefile` clone a symbolic link itself rather than what it points at, as Darwin's `clonefile.h` defines it.
    const CLONE_NOFOLLOW: u32 = 0x0001;

    /// Clones the file, folder or symbolic link `from`, of the kind `_kind`, as `to`, which must not exist; a folder is cloned whole in one call.
    ///
    /// # Errors
    ///
    /// Fails with the error `clonefile` sets, or when a path holds a NUL byte.
    pub(super) fn clone_entry(from: &Path, to: &Path, _kind: FileType) -> io::Result<()> {
        let from = CString::new(from.as_os_str().as_bytes())?;
        let to = CString::new(to.as_os_str().as_bytes())?;
        let status = unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), CLONE_NOFOLLOW) };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Whether the error number `code` says the volume cannot clone.
    pub(super) fn unsupported_code(code: i32) -> bool {
        matches!(code, libc::ENOTSUP | libc::EXDEV)
    }
}

#[cfg(target_os = "linux")]
/// Cloning with the `FICLONE` ioctl, file by file.
mod imp {
    use std::fs::{self, File, FileTimes, FileType};
    use std::io;
    use std::os::fd::AsRawFd as _;
    use std::path::Path;

    /// The `FICLONE` ioctl request, `_IOW(0x94, 9, int)`.
    const FICLONE: u32 = 0x4004_9409;

    /// Clones the file, folder or symbolic link `from`, of the kind `kind`, as `to`, which must not exist: a folder is made and filled entry by entry, then given `from`'s permissions and modification time.
    ///
    /// # Errors
    ///
    /// Fails with the error of the first call that fails.
    pub(super) fn clone_entry(from: &Path, to: &Path, kind: FileType) -> io::Result<()> {
        if kind.is_symlink() {
            return std::os::unix::fs::symlink(fs::read_link(from)?, to);
        }
        if kind.is_file() {
            return clone_file(from, to);
        }
        fs::create_dir(to)?;
        super::clone_entries(from, to, &[])?;
        let meta = fs::symlink_metadata(from)?;
        fs::set_permissions(to, meta.permissions())?;
        let _ = File::open(to).and_then(|folder| folder.set_times(times(&meta)));
        Ok(())
    }

    /// Clones the regular file `from` as the new file `to`, with `from`'s permissions and times; `to` is removed again when cloning fails.
    ///
    /// # Errors
    ///
    /// Fails with the error the ioctl sets, or of opening either file.
    fn clone_file(from: &Path, to: &Path) -> io::Result<()> {
        let source = File::open(from)?;
        let target = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(to)?;
        let status = unsafe {
            libc::ioctl(
                target.as_raw_fd(),
                FICLONE as libc::Ioctl,
                source.as_raw_fd(),
            )
        };
        if status != 0 {
            let error = io::Error::last_os_error();
            drop(target);
            let _ = fs::remove_file(to);
            return Err(error);
        }
        let meta = source.metadata()?;
        target.set_permissions(meta.permissions())?;
        let _ = target.set_times(times(&meta));
        Ok(())
    }

    /// The access and modification times of `meta`, to give a clone.
    fn times(meta: &fs::Metadata) -> FileTimes {
        let mut times = FileTimes::new();
        if let Ok(accessed) = meta.accessed() {
            times = times.set_accessed(accessed);
        }
        if let Ok(modified) = meta.modified() {
            times = times.set_modified(modified);
        }
        times
    }

    /// Whether the error number `code` says the file system cannot clone.
    pub(super) fn unsupported_code(code: i32) -> bool {
        matches!(
            code,
            libc::EXDEV | libc::EOPNOTSUPP | libc::ENOTTY | libc::EINVAL | libc::ENOSYS
        )
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
/// No cloning, on platforms crab knows no copy-on-write call for.
mod imp {
    use std::fs::FileType;
    use std::io;
    use std::path::Path;

    /// Fails, since nothing can be cloned here.
    ///
    /// # Errors
    ///
    /// Always fails with [`io::ErrorKind::Unsupported`].
    pub(super) fn clone_entry(_from: &Path, _to: &Path, _kind: FileType) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "copy-on-write clones are not made on this platform",
        ))
    }

    /// Whether the error number `code` says the file system cannot clone: never, since [`clone_entry`] says so by its kind.
    pub(super) fn unsupported_code(_code: i32) -> bool {
        false
    }
}

#[cfg(test)]
/// Tests of cloning folders.
mod tests {
    use super::*;

    /// A folder is cloned with its nested files, a symbolic link stays a link, and a skipped entry is left out; where clones cannot be made the error says so.
    #[test]
    fn folders_are_cloned_without_skipped_entries() {
        let root = crate::testkit::scratch("cow-clone");
        let from = root.join("from");
        let to = root.join("to");
        std::fs::create_dir_all(from.join("nested")).unwrap();
        std::fs::create_dir_all(from.join(".git")).unwrap();
        std::fs::write(from.join("a.txt"), "alpha").unwrap();
        std::fs::write(from.join("nested/b.txt"), "beta").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("a.txt", from.join("link")).unwrap();
        match clone_entries(&from, &to, &[OsStr::new(".git")]) {
            Err(error) if unsupported(&error) => return,
            other => other.unwrap(),
        }
        assert!(!to.join(".git").exists());
        assert_eq!(std::fs::read_to_string(to.join("a.txt")).unwrap(), "alpha");
        assert_eq!(
            std::fs::read_to_string(to.join("nested/b.txt")).unwrap(),
            "beta"
        );
        #[cfg(unix)]
        assert_eq!(
            std::fs::read_link(to.join("link")).unwrap(),
            Path::new("a.txt")
        );
        std::fs::write(to.join("a.txt"), "changed").unwrap();
        assert_eq!(
            std::fs::read_to_string(from.join("a.txt")).unwrap(),
            "alpha"
        );
    }
}
