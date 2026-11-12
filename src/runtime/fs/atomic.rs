//! Same-directory replacement on Unix. All syscalls run on a file worker.
use super::{worker::FileHandle, FsError};
use std::{
    ffi::{CStr, CString},
    fs::File,
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};

pub(super) fn replace(
    path: &Path,
    bytes: &[u8],
    cancelled: &dyn Fn() -> bool,
) -> Result<(), FsError> {
    check_cancelled(cancelled)?;
    let target = path
        .as_os_str()
        .as_bytes()
        .rsplit(|byte| *byte == b'/')
        .next()
        .unwrap_or_default();
    if matches!(target, b"" | b"." | b"..") {
        return Err(FsError::new(5, "Atomic write requires a file name"));
    }
    let target = CString::new(target).map_err(|_| FsError::new(5, "Invalid file name"))?;
    let parent = path
        .parent()
        .ok_or_else(|| FsError::new(5, "Atomic write requires a parent directory"))?;
    let parent = CString::new(parent.as_os_str().as_bytes())
        .map_err(|_| FsError::new(5, "Invalid parent directory"))?;
    let directory = open_directory(&parent)?;
    let (temporary, mut file) = Temporary::create(&directory, &target)?;
    file.write_all(bytes)?;
    FileHandle::close(Box::new(file))?;
    // After this checkpoint the rename may win a race with cancellation.
    check_cancelled(cancelled)?;
    temporary.publish(&target)
}

fn check_cancelled(cancelled: &dyn Fn() -> bool) -> Result<(), FsError> {
    if cancelled() {
        Err(FsError::new(7, "Atomic write was cancelled"))
    } else {
        Ok(())
    }
}

fn open_directory(path: &CStr) -> io::Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    let access = libc::O_PATH;
    #[cfg(target_vendor = "apple")]
    let access = libc::O_SEARCH;
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    let access = libc::O_RDONLY;
    // SAFETY: The path is NUL-terminated. A successful open yields a fresh descriptor.
    let fd = unsafe { libc::open(path.as_ptr(), access | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

struct Temporary<'a> {
    directory: &'a OwnedFd,
    name: CString,
    published: bool,
}

impl<'a> Temporary<'a> {
    fn create(directory: &'a OwnedFd, target: &CStr) -> Result<(Self, File), FsError> {
        for _ in 0..16 {
            let mut random = [0; 16];
            getrandom::fill(&mut random).map_err(|error| {
                FsError::new(10, format!("Cannot name temporary file: {error}"))
            })?;
            let name = CString::new(format!(".joky-write-{:032x}", u128::from_ne_bytes(random)))
                .expect("hex file name contains no NUL");
            if name.as_c_str() == target {
                continue;
            }
            // SAFETY: The directory and name are live. O_EXCL prevents opening
            // an existing entry and a successful call transfers the new fd.
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                    0o600 as libc::c_uint,
                )
            };
            if fd >= 0 {
                return Ok((
                    Self {
                        directory,
                        name,
                        published: false,
                    },
                    unsafe { File::from_raw_fd(fd) },
                ));
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        }
        Err(FsError::new(6, "Cannot reserve a temporary file name"))
    }

    fn publish(mut self, target: &CStr) -> Result<(), FsError> {
        let directory = self.directory.as_raw_fd();
        // SAFETY: Both names are NUL-terminated and relative to the live directory.
        if unsafe { libc::renameat(directory, self.name.as_ptr(), directory, target.as_ptr()) } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
        self.published = true;
        Ok(())
    }
}

impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if !self.published {
            // SAFETY: The directory and temporary name outlive this cleanup.
            unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::aot::tests::ScratchDir;
    use std::{cell::Cell, fs, sync::Barrier, thread};

    #[test]
    fn atomic_write_cleans_temporary_files_after_cancel_and_failed_rename() {
        let work = ScratchDir::new("atomic-cleanup");
        let target = work.path().join("target");
        fs::write(&target, b"old").unwrap();
        let checkpoint = Cell::new(0);
        let error = replace(&target, b"new", &|| {
            checkpoint.set(checkpoint.get() + 1);
            if checkpoint.get() == 2 {
                assert_eq!(fs::read_dir(work.path()).unwrap().count(), 2);
                true
            } else {
                false
            }
        })
        .unwrap_err();
        assert_eq!(error.code(), 7);
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert_eq!(fs::read_dir(work.path()).unwrap().count(), 1);

        let directory = work.path().join("directory");
        fs::create_dir(&directory).unwrap();
        assert!(replace(&directory, b"new", &|| false).is_err());
        assert!(directory.is_dir());
        assert_eq!(fs::read_dir(work.path()).unwrap().count(), 2);
    }

    #[test]
    fn atomic_write_readers_observe_complete_versions() {
        let work = ScratchDir::new("atomic-readers");
        let target = work.path().join("target");
        let old = vec![b'a'; 65536];
        let new = vec![b'b'; 65535];
        fs::write(&target, &old).unwrap();
        let start = Barrier::new(2);
        thread::scope(|scope| {
            scope.spawn(|| {
                start.wait();
                for _ in 0..256 {
                    let bytes = fs::read(&target).unwrap();
                    assert!(bytes == old || bytes == new);
                }
            });
            start.wait();
            for i in 0..32 {
                let bytes = if i % 2 == 0 { &new } else { &old };
                replace(&target, bytes, &|| false).unwrap();
            }
        });
        assert_eq!(fs::read_dir(work.path()).unwrap().count(), 1);
    }
}
