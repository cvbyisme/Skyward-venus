//! Android mksh redirects only a single-digit descriptor with >&$OUTFD.
//! The UI commonly has more than ten files open. Reserve fd 3 in the
//! child, leaving the parent's descriptors and close-on-exec flags intact.
use std::io;
use std::os::fd::RawFd;

pub const REQUEST_FD: RawFd = 3;

/// Only async-signal-safe system calls: used by Command::pre_exec.
pub fn inherit(source: RawFd) -> io::Result<()> {
    if source != REQUEST_FD {
        if unsafe { libc::dup2(source, REQUEST_FD) } == -1 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::close(source) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    if unsafe { libc::fcntl(REQUEST_FD, libc::F_SETFD, 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::time::Duration;

    #[test]
    fn shell_can_report_progress_when_ui_has_many_open_files() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        reader.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let fd = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 40) };
        assert!(fd >= 40);
        let held = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut child = Command::new("sh");
        child.args(["-c", "printf 'ui_print progress works\\n' >&\"$1\"; test ! -e /proc/self/fd/\"$2\"", "sh", "3", &fd.to_string()]);
        unsafe { child.pre_exec(move || inherit(fd)); }
        assert!(child.status().unwrap().success());
        let expected = b"ui_print progress works\n";
        let mut got = vec![0; expected.len()];
        reader.read_exact(&mut got).unwrap();
        assert_eq!(got, expected);
        assert_ne!(unsafe { libc::fcntl(held.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC, 0);
    }
}
