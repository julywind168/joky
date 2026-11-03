//! Recoverable stderr writes, called with the Rust stderr lock held.
#[cfg(not(unix))]
use std::io::Write;

#[cfg(target_vendor = "apple")]
pub(super) fn write_log(bytes: &[u8]) -> std::io::Result<()> {
    use std::{
        fs::File,
        io::{self, Write},
        os::fd::{AsRawFd, FromRawFd},
    };
    // Darwin <sys/fcntl.h>; libc does not expose these two commands.
    const F_SETNOSIGPIPE: libc::c_int = 73;
    const F_GETNOSIGPIPE: libc::c_int = 74;
    // Darwin can deliver pipe signals to another thread. Pin the open file
    // and temporarily suppress SIGPIPE on it, restoring its previous flag.
    let fd = unsafe { libc::fcntl(libc::STDERR_FILENO, libc::F_DUPFD_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let previous = unsafe { libc::fcntl(fd, F_GETNOSIGPIPE) };
    if previous < 0 {
        return Err(io::Error::last_os_error());
    }
    if previous != 0 {
        return file.write_all(bytes);
    }
    if unsafe { libc::fcntl(fd, F_SETNOSIGPIPE, 1) } < 0 {
        return Err(io::Error::last_os_error());
    }
    struct Restore(File);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                libc::fcntl(self.0.as_raw_fd(), F_SETNOSIGPIPE, 0);
            }
        }
    }
    Restore(file).0.write_all(bytes)
}
#[cfg(windows)]
pub(super) fn write_log(bytes: &[u8]) -> std::io::Result<()> {
    use std::{fs::File, os::windows::io::AsHandle};
    let mut file = File::from(std::io::stderr().as_handle().try_clone_to_owned()?);
    file.write_all(bytes)
}
#[cfg(not(any(unix, windows)))]
pub(super) fn write_log(bytes: &[u8]) -> std::io::Result<()> {
    let mut stream = std::io::stderr().lock();
    stream.write_all(bytes)?;
    stream.flush()
}

// Suppress only the SIGPIPE raised by this write on this thread. Embedded
// callers keep their process-wide signal disposition and pending signals.
#[cfg(all(unix, not(target_vendor = "apple")))]
pub(super) fn write_log(bytes: &[u8]) -> std::io::Result<()> {
    use std::{io, mem::MaybeUninit};
    struct Mask(libc::sigset_t);
    impl Drop for Mask {
        fn drop(&mut self) {
            unsafe {
                libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut());
            }
        }
    }
    unsafe {
        let mut signals = MaybeUninit::uninit();
        libc::sigemptyset(signals.as_mut_ptr());
        let mut signals = signals.assume_init();
        libc::sigaddset(&mut signals, libc::SIGPIPE);
        let mut old = MaybeUninit::uninit();
        let error = libc::pthread_sigmask(libc::SIG_BLOCK, &signals, old.as_mut_ptr());
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        let _mask = Mask(old.assume_init());
        let pending = || -> io::Result<bool> {
            let mut set = MaybeUninit::uninit();
            if libc::sigpending(set.as_mut_ptr()) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(libc::sigismember(set.as_ptr(), libc::SIGPIPE) == 1)
        };
        let already_pending = pending()?;
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = libc::write(libc::STDERR_FILENO, rest.as_ptr().cast(), rest.len());
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if e.kind() == io::ErrorKind::BrokenPipe && !already_pending && pending()? {
                    let mut signal = 0;
                    libc::sigwait(&signals, &mut signal);
                }
                return Err(e);
            }
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            rest = &rest[n as usize..];
        }
        Ok(())
    }
}
