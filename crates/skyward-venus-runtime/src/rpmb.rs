use crate::common::*;
use anyhow::{Result, bail, ensure};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::Path,
};
const NODE: &str = "/dev/0:0:0:49476";
fn rule_line(input: &str) -> Result<bool> {
    ensure!(input.len() < 1024, "RPMB rule too long");
    let parts: Vec<_> = input
        .split('#')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect();
    if parts.first() != Some(&NODE) {
        return Ok(false);
    }
    ensure!(
        parts == [NODE, "0600", "system", "system"],
        "RPMB vendor rule unsupported"
    );
    Ok(true)
}
fn valid_device(m: &fs::Metadata, major: u32, minor: u32) -> bool {
    m.file_type().is_char_device()
        && major > 0
        && libc::major(m.rdev()) as u32 == major
        && libc::minor(m.rdev()) as u32 == minor
}
fn vendor_rule() -> Result<()> {
    let cached = Path::new("/skyward/venus-recovery.txt").exists()
        && Path::new("/skyward/run/hal/cache/ready").exists();
    if cached {
        cache_ready()?;
    }
    let path = if cached {
        "/skyward/run/hal/cache/vendor/ueventd.rc"
    } else {
        "/mnt/skyward-ro/vendor/ueventd.rc"
    };
    let mut count = 0;
    for line in fs::read_to_string(path)?.split_inclusive('\n') {
        if rule_line(line)? {
            count += 1;
        }
    }
    ensure!(count == 1, "RPMB rule missing or ambiguous");
    Ok(())
}
pub fn run(args: &[OsString]) -> Result<()> {
    ensure!(
        args.len() == 1 && (args[0] == "--apply" || args[0] == "--check"),
        "use km41-rpmb --check or --apply"
    );
    recovery()?;
    vendor_rule()?;
    let sys = fs::read_to_string("/sys/class/bsg/0:0:0:49476/dev")?;
    let (a, b) = sys
        .trim()
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("RPMB sysfs identity"))?;
    let (ma, mi) = (a.parse::<u32>()?, b.parse::<u32>()?);
    let before = fs::symlink_metadata(NODE)?;
    ensure!(
        valid_device(&before, ma, mi),
        "RPMB node is not expected character device"
    );
    println!(
        "VENUS_RPMB_NODE_BEFORE uid={} gid={} mode={:04o} identity=verified",
        before.uid(),
        before.gid(),
        before.mode() & 0o7777
    );
    if args[0] == "--apply"
        && (before.uid() != 1000 || before.gid() != 1000 || before.mode() & 0o7777 != 0o600)
    {
        let f = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(NODE)?;
        let opened = f.metadata()?;
        ensure!(
            valid_device(&opened, ma, mi)
                && opened.ino() == before.ino()
                && opened.dev() == before.dev(),
            "RPMB node changed during validation"
        );
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
        cv(unsafe { libc::fchown(f.as_raw_fd(), 1000, 1000) })?;
        println!(
            "VENUS_RPMB_PERMISSIONS_APPLIED source=installed_vendor owner=system:system mode=0600"
        );
    }
    let after = fs::symlink_metadata(NODE)?;
    ensure!(valid_device(&after, ma, mi), "RPMB node changed");
    system_access()?;
    ensure!(
        after.uid() == 1000 && after.gid() == 1000 && after.mode() & 0o7777 == 0o600,
        "VENUS_RPMB_DAC_MISMATCH expected=system:system:0600"
    );
    Ok(())
}
fn system_access() -> Result<()> {
    let pid = unsafe { libc::fork() };
    cv(pid)?;
    if pid == 0 {
        // No allocation or Rust I/O in the post-fork child. Opening and closing
        // the verified device is the entire probe; no read/write/ioctl occurs.
        unsafe {
            if libc::prctl(libc::PR_SET_KEEPCAPS, 0, 0, 0, 0) != 0
                || libc::setgroups(0, std::ptr::null()) != 0
                || libc::setresgid(1000, 1000, 1000) != 0
                || libc::setresuid(1000, 1000, 1000) != 0
            {
                libc::_exit(1);
            }
            let fd = libc::open(
                c"/dev/0:0:0:49476".as_ptr(),
                libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            );
            libc::_exit(if fd < 0 || libc::close(fd) != 0 { 1 } else { 0 });
        }
    }
    let mut status = 0;
    loop {
        if unsafe { libc::waitpid(pid, &mut status, 0) } >= 0 {
            break;
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e.into());
        }
    }
    if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
        bail!("VENUS_RPMB_ACCESS_FAILED uid=1000 gid=1000 no_device_io=true");
    }
    println!("VENUS_RPMB_ACCESS_OK uid=1000 gid=1000 mode=0600 open_close_only=true ioctl=false");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_vendor_rule_only() {
        assert!(rule_line(&format!("{NODE} 0600 system system\n")).unwrap());
        assert!(rule_line(&format!("  {NODE}\t0600\tsystem\tsystem # installed\n")).unwrap());
        assert!(!rule_line(&format!("# {NODE} 0600 system system")).unwrap());
        assert!(!rule_line("/dev/another 0600 system system").unwrap());
        for rule in [
            "0666 system system",
            "0600 root root",
            "0600 system",
            "0600 system system extra",
        ] {
            assert!(rule_line(&format!("{NODE} {rule}")).is_err());
        }
    }
}
