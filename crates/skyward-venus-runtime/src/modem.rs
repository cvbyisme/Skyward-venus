use crate::common::*;
use anyhow::{Result, ensure};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::{
        fd::IntoRawFd,
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
struct Receipt {
    path: &'static str,
    identity: fs::Metadata,
}
impl Receipt {
    fn remove(&self) -> Result<()> {
        let m = fs::symlink_metadata(self.path)?;
        ensure!(
            m.is_file() && m.dev() == self.identity.dev() && m.ino() == self.identity.ino(),
            "modem receipt ownership changed"
        );
        fs::remove_file(self.path)?;
        Ok(())
    }
}
impl Drop for Receipt {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}
pub fn hold() -> Result<()> {
    root()?;
    ensure!(
        Path::new("/skyward/venus-recovery.txt").is_file(),
        "requires integrated Recovery"
    );
    let ready = "/skyward/run/state/modem-held.ready";
    ensure!(
        fs::symlink_metadata(ready).is_err_and(|e| e.kind() == io::ErrorKind::NotFound),
        "existing modem owner"
    );
    let before = fs::symlink_metadata("/dev/subsys_modem")?;
    ensure!(
        before.file_type().is_char_device(),
        "modem is not a real character device"
    );
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = stop as *const () as usize;
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGALRM] {
        cv(unsafe { libc::sigaction(sig, &action, std::ptr::null_mut()) })?;
    }
    unsafe {
        libc::alarm(45);
    }
    println!("MODEM_PHASE open kernel subsystem reference");
    io::stdout().flush()?;
    // Use libc::open directly: std's EINTR retry would defeat the alarm if
    // the kernel's subsystem open blocks indefinitely.
    let raw = unsafe {
        libc::open(
            c"/dev/subsys_modem".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    unsafe {
        libc::alarm(0);
    }
    cv(raw)?;
    use std::os::fd::FromRawFd;
    let fd = unsafe { fs::File::from_raw_fd(raw) };
    let after = fd.metadata()?;
    ensure!(
        !STOP.load(Ordering::Relaxed)
            && after.file_type().is_char_device()
            && before.rdev() == after.rdev(),
        "modem open interrupted or identity changed"
    );
    let mut blocked = unsafe { std::mem::zeroed::<libc::sigset_t>() };
    let mut previous = blocked;
    unsafe {
        libc::sigemptyset(&mut blocked);
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGALRM] {
            libc::sigaddset(&mut blocked, sig);
        }
    }
    cv(unsafe { libc::sigprocmask(libc::SIG_BLOCK, &blocked, &mut previous) })?;
    ensure!(!STOP.load(Ordering::Relaxed), "modem stop requested");
    let mut waiting = previous;
    unsafe {
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGALRM] {
            libc::sigdelset(&mut waiting, sig);
        }
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(ready)?;
    let receipt = Receipt {
        path: ready,
        identity: file.metadata()?,
    };
    writeln!(file, "{}", std::process::id())?;
    cv(unsafe { libc::close(file.into_raw_fd()) })?;
    println!("MODEM_PHASE subsystem reference held; query QMI separately for readiness");
    io::stdout().flush()?;
    while !STOP.load(Ordering::Relaxed) {
        unsafe {
            libc::sigsuspend(&waiting);
        }
    }
    cv(unsafe { libc::close(fd.into_raw_fd()) })?;
    receipt.remove()?;
    std::mem::forget(receipt);
    cv(unsafe { libc::sigprocmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) })?;
    println!("MODEM_PHASE subsystem reference released");
    Ok(())
}
