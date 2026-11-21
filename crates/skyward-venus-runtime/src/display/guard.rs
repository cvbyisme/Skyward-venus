//! Seccomp guards for isolated display experiments, installed before vendor
//! constructors or worker threads. They do not claim to sandbox hostile code.
use anyhow::{Result, ensure};
#[derive(Clone, Copy)]
#[repr(C)]
struct Instruction {
    code: u16,
    yes: u8,
    no: u8,
    value: u32,
}
#[repr(C)]
struct Program {
    length: u16,
    filter: *const Instruction,
}
const LOAD: u16 = 0x20;
const EQ: u16 = 0x15;
const SET: u16 = 0x45;
const RETURN: u16 = 0x06;
const ALLOW: u32 = 0x7fff0000;
const DENY: u32 = 0x50000 | libc::EPERM as u32;
// Android AArch64 libc omits these names although asm-generic numbers exist.
#[cfg(all(target_os = "android", target_arch = "aarch64"))]
const SYS_TRUNCATE: libc::c_long = 45;
#[cfg(all(target_os = "android", target_arch = "aarch64"))]
const SYS_FTRUNCATE: libc::c_long = 46;
#[cfg(not(all(target_os = "android", target_arch = "aarch64")))]
const SYS_TRUNCATE: libc::c_long = libc::SYS_truncate;
#[cfg(not(all(target_os = "android", target_arch = "aarch64")))]
const SYS_FTRUNCATE: libc::c_long = libc::SYS_ftruncate;
#[cfg(target_arch = "aarch64")]
const ARCH: u32 = 0xc00000b7;
#[cfg(target_arch = "x86_64")]
const ARCH: u32 = 0xc000003e;
fn stmt(code: u16, value: u32) -> Instruction {
    Instruction {
        code,
        yes: 0,
        no: 0,
        value,
    }
}
fn jump(code: u16, value: u32, yes: u8, no: u8) -> Instruction {
    Instruction {
        code,
        yes,
        no,
        value,
    }
}
#[derive(Clone, Copy)]
pub enum Policy {
    InitializeBinder,
    NoIoctl,
    BinderOnly(i32),
    BinderPair(i32, i32),
}
fn filter(policy: Policy) -> Vec<Instruction> {
    let mut f = vec![
        stmt(LOAD, 4),
        jump(EQ, ARCH, 1, 0),
        stmt(RETURN, 0x80000000),
        stmt(LOAD, 0),
    ];
    let mut ioctl = Vec::new();
    match policy {
        Policy::InitializeBinder => {
            ioctl.extend([
                stmt(LOAD, 24),
                jump(EQ, 0xc0046209, 2, 0),
                jump(EQ, 0x40046205, 1, 0),
                stmt(RETURN, DENY),
                stmt(RETURN, ALLOW),
            ]);
        }
        Policy::NoIoctl => ioctl.push(stmt(RETURN, DENY)),
        Policy::BinderOnly(fd) => {
            ioctl.extend([
                stmt(LOAD, 16),
                jump(EQ, fd as u32, 1, 0),
                stmt(RETURN, DENY),
                stmt(LOAD, 24),
                jump(EQ, 0xc0306201, 2, 0),
                jump(EQ, 0x40046208, 1, 0),
                stmt(RETURN, DENY),
                stmt(RETURN, ALLOW),
            ]);
        }
        Policy::BinderPair(fd0, fd1) => {
            ioctl.extend([
                stmt(LOAD, 16),
                jump(EQ, fd0 as u32, 2, 0),
                jump(EQ, fd1 as u32, 1, 0),
                stmt(RETURN, DENY),
                stmt(LOAD, 24),
                jump(EQ, 0xc0306201, 2, 0),
                jump(EQ, 0x40046208, 1, 0),
                stmt(RETURN, DENY),
                stmt(RETURN, ALLOW),
            ]);
        }
    }
    f.push(jump(EQ, libc::SYS_ioctl as u32, 0, ioctl.len() as u8));
    f.extend(ioctl);
    if matches!(policy, Policy::InitializeBinder) {
        f.push(stmt(RETURN, ALLOW));
        return f;
    }
    // Reload syscall nr after the ioctl path's argument loads.
    f.push(stmt(LOAD, 0));
    let mut denied = vec![
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_unlinkat,
        libc::SYS_renameat,
        libc::SYS_renameat2,
        libc::SYS_linkat,
        libc::SYS_symlinkat,
        libc::SYS_mkdirat,
        libc::SYS_mknodat,
        SYS_TRUNCATE,
        SYS_FTRUNCATE,
        libc::SYS_fchmod,
        libc::SYS_fchmodat,
        libc::SYS_fchown,
        libc::SYS_fchownat,
        libc::SYS_setxattr,
        libc::SYS_lsetxattr,
        libc::SYS_fsetxattr,
        libc::SYS_removexattr,
        libc::SYS_lremovexattr,
        libc::SYS_fremovexattr,
        libc::SYS_openat2,
    ];
    if matches!(policy, Policy::BinderOnly(_) | Policy::BinderPair(_, _)) {
        denied.extend([
            libc::SYS_socket,
            libc::SYS_socketpair,
            libc::SYS_connect,
            libc::SYS_bind,
            libc::SYS_listen,
            libc::SYS_accept,
            libc::SYS_accept4,
        ]);
    }
    #[cfg(target_arch = "x86_64")]
    denied.extend([
        libc::SYS_unlink,
        libc::SYS_rename,
        libc::SYS_link,
        libc::SYS_symlink,
        libc::SYS_mkdir,
        libc::SYS_mknod,
        libc::SYS_chmod,
        libc::SYS_chown,
        libc::SYS_lchown,
        libc::SYS_creat,
    ]);
    for nr in denied {
        f.extend([jump(EQ, nr as u32, 0, 1), stmt(RETURN, DENY)]);
    }
    {
        let (nr, offset) = (libc::SYS_openat, 32);
        f.extend([
            jump(EQ, nr as u32, 0, 4),
            stmt(LOAD, offset),
            jump(
                SET,
                (libc::O_WRONLY | libc::O_RDWR | libc::O_CREAT | libc::O_TRUNC | libc::O_APPEND)
                    as u32,
                0,
                1,
            ),
            stmt(RETURN, DENY),
            stmt(RETURN, ALLOW),
        ]);
    }
    #[cfg(target_arch = "x86_64")]
    f.extend([
        jump(EQ, libc::SYS_open as u32, 0, 4),
        stmt(LOAD, 24),
        jump(
            SET,
            (libc::O_WRONLY | libc::O_RDWR | libc::O_CREAT | libc::O_TRUNC | libc::O_APPEND) as u32,
            0,
            1,
        ),
        stmt(RETURN, DENY),
        stmt(RETURN, ALLOW),
    ]);
    f.push(stmt(RETURN, ALLOW));
    f
}
pub fn install(policy: Policy) -> Result<()> {
    let instructions = filter(policy);
    let program = Program {
        length: instructions.len().try_into()?,
        filter: instructions.as_ptr(),
    };
    ensure!(
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == 0,
        "display no_new_privs: {}",
        std::io::Error::last_os_error()
    );
    ensure!(
        unsafe { libc::prctl(libc::PR_SET_SECCOMP, 2, &program) } == 0,
        "display seccomp: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, os::fd::AsRawFd, process::Command};
    fn errno() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap()
    }
    #[test]
    fn inherited_display_io_guards_enforce_real_syscalls() {
        const KEY: &str = "SKYWARD_DISPLAY_GUARD_TEST";
        if let Ok(mode) = std::env::var(KEY) {
            let allowed = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")
                .unwrap();
            let other = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")
                .unwrap();
            if mode == "init" {
                install(Policy::InitializeBinder).unwrap();
                for request in [0xc0046209u32, 0x40046205] {
                    assert_eq!(
                        unsafe { libc::ioctl(allowed.as_raw_fd(), request as libc::Ioctl, 0) },
                        -1
                    );
                    assert_eq!(errno(), libc::ENOTTY);
                }
                assert_eq!(
                    unsafe { libc::ioctl(allowed.as_raw_fd(), 0xc0306201u32 as libc::Ioctl, 0) },
                    -1
                );
                assert_eq!(errno(), libc::EPERM);
                return;
            }
            install(if mode == "binder" {
                Policy::BinderOnly(allowed.as_raw_fd())
            } else {
                Policy::NoIoctl
            })
            .unwrap();
            for request in [0xc0306201u32, 0x40046208] {
                assert_eq!(
                    unsafe { libc::ioctl(allowed.as_raw_fd(), request as libc::Ioctl, 0) },
                    -1
                );
                assert_eq!(
                    errno(),
                    if mode == "binder" {
                        libc::ENOTTY
                    } else {
                        libc::EPERM
                    }
                );
            }
            for fd in [allowed.as_raw_fd(), other.as_raw_fd()] {
                assert_eq!(
                    unsafe { libc::ioctl(fd, 0xc0046209u32 as libc::Ioctl, 0) },
                    -1
                );
                assert_eq!(errno(), libc::EPERM);
            }
            assert_eq!(
                OpenOptions::new()
                    .write(true)
                    .open("/dev/null")
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::EPERM)
            );
            assert_eq!(
                OpenOptions::new()
                    .read(true)
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open("/tmp/display-guard-must-not-create")
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::EPERM)
            );
            assert!(std::fs::File::open("/dev/null").is_ok());
            assert_eq!(
                Command::new("/bin/true")
                    .status()
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::EPERM)
            );
            std::thread::spawn(move || {
                assert_eq!(
                    unsafe { libc::ioctl(other.as_raw_fd(), 0xc0306201u32 as libc::Ioctl, 0) },
                    -1
                );
                assert_eq!(errno(), libc::EPERM);
            })
            .join()
            .unwrap();
            return;
        }
        for mode in ["init", "none", "binder"] {
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "display::guard::tests::inherited_display_io_guards_enforce_real_syscalls",
                    "--nocapture",
                ])
                .env(KEY, mode)
                .status()
                .unwrap();
            assert!(status.success(), "guard mode={mode}");
        }
    }
}
