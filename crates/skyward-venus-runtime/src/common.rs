use anyhow::{Context, Result, ensure};
use std::{
    ffi::{CString, OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::Path,
    process::Command,
};

pub fn cv(value: libc::c_int) -> io::Result<()> {
    if value == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
pub fn c(value: impl AsRef<OsStr>) -> Result<CString> {
    Ok(CString::new(value.as_ref().as_bytes())?)
}
pub fn no_args(args: &[OsString]) -> Result<()> {
    ensure!(args.is_empty(), "unexpected arguments");
    Ok(())
}
pub fn root() -> Result<()> {
    ensure!(unsafe { libc::geteuid() } == 0, "requires root");
    Ok(())
}
pub fn recovery() -> Result<()> {
    root()?;
    ensure!(
        fs::read_to_string("/skyward/probe-entryfix.txt")?
            .lines()
            .next()
            == Some("venus-entryfix-v1"),
        "requires Venus temporary Recovery"
    );
    Ok(())
}
pub fn directory(path: impl AsRef<Path>, mode: u32) -> Result<()> {
    let path = path.as_ref();
    match fs::symlink_metadata(path) {
        Ok(m) => ensure!(m.is_dir(), "{} is not a real directory", path.display()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(path)?,
        Err(e) => return Err(e.into()),
    }
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    f.set_permissions(fs::Permissions::from_mode(mode))?;
    Ok(())
}
#[allow(clippy::unnecessary_cast)] // Android uses unsigned f_type; host Linux uses signed.
pub fn fs_type(path: impl AsRef<Path>) -> Result<u64> {
    let p = c(path.as_ref())?;
    let mut s = std::mem::MaybeUninit::<libc::statfs>::uninit();
    cv(unsafe { libc::statfs(p.as_ptr(), s.as_mut_ptr()) })?;
    Ok(unsafe { s.assume_init() }.f_type as u64)
}
pub fn mount(
    source: Option<&Path>,
    target: &Path,
    kind: Option<&str>,
    flags: libc::c_ulong,
    data: Option<&str>,
) -> Result<()> {
    let s = source.map(c).transpose()?;
    let t = c(target)?;
    let k = kind.map(c).transpose()?;
    let d = data.map(c).transpose()?;
    cv(unsafe {
        libc::mount(
            s.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
            t.as_ptr(),
            k.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
            flags,
            d.as_ref().map_or(std::ptr::null(), |v| v.as_ptr().cast()),
        )
    })
    .with_context(|| format!("mount {}", target.display()))
}
pub fn bind(source: &Path, target: &Path, recursive: bool, readonly: bool) -> Result<()> {
    mount(
        Some(source),
        target,
        None,
        libc::MS_BIND | if recursive { libc::MS_REC } else { 0 },
        None,
    )?;
    if readonly {
        mount(
            None,
            target,
            None,
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV,
            None,
        )?;
    }
    Ok(())
}
pub fn private_mounts() -> Result<()> {
    cv(unsafe { libc::unshare(libc::CLONE_NEWNS) })?;
    mount(
        None,
        Path::new("/"),
        None,
        libc::MS_REC | libc::MS_PRIVATE,
        None,
    )
}
pub fn chroot(path: &Path) -> Result<()> {
    cv(unsafe { libc::chroot(c(path)?.as_ptr()) })?;
    std::env::set_current_dir("/")?;
    Ok(())
}
pub const PRIVATE_SOURCES: &[&str] = &[
    "/mnt/skyward-ro/system",
    "/mnt/skyward-ro/system_ext",
    "/mnt/skyward-ro/vendor",
    "/mnt/skyward-ro/odm",
    "/mnt/skyward-ro/product",
    "/mnt/skyward-ro/mi_ext",
    "/mnt/skyward-ro/system_dlkm",
    "/mnt/skyward-ro/vendor_dlkm",
    "/mnt/skyward-ro/odm_dlkm",
    "/system/etc/firmware/wlan/qca_cld",
    "/vendor/firmware/wlan/qca_cld",
    "/vendor/firmware_mnt",
    "/vendor/firmware",
    "/sdcard",
    "/data",
    "/metadata",
    "/persist",
    "/mnt/vendor/persist",
    "/cache",
];
/// Only after unshare + MS_PRIVATE. Never lazy or forced; shared mounts survive.
pub fn detach_private_sources() -> Result<()> {
    detach_with(|path| cv(unsafe { libc::umount(c(path).expect("constant path").as_ptr()) }))
}
fn detach_with(mut unmount: impl FnMut(&str) -> io::Result<()>) -> Result<()> {
    for path in PRIVATE_SOURCES {
        if let Err(e) = unmount(path)
            && !matches!(e.raw_os_error(), Some(libc::EINVAL | libc::ENOENT))
        {
            return Err(e).with_context(|| format!("detach private source {path}"));
        }
    }
    Ok(())
}
pub fn cache_ready() -> Result<()> {
    let path = Path::new("/skyward/run/hal/cache");
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && m.uid() == 0 && m.mode() & 0o077 == 0 && fs_type(path)? == 0x01021994,
        "private RAM runtime cache"
    );
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    ensure!(
        boot.len() == 37 && boot.ends_with('\n'),
        "cache boot identity format"
    );
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path.join("ready"))?;
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.uid() == 0 && m.mode() & 0o022 == 0,
        "cache receipt identity"
    );
    let mut got = String::new();
    (&mut f).take(128).read_to_string(&mut got)?;
    ensure!(
        got == format!("venus-runtime-cache-v1\nboot={boot}"),
        "stale runtime cache"
    );
    Ok(())
}
pub fn exec(command: &mut Command) -> Result<()> {
    Err(command.exec()).context("exec Venus service")
}
pub fn own(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<()> {
    let p = c(path)?;
    cv(unsafe { libc::chown(p.as_ptr(), uid, gid) })?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}
pub fn regular_target(path: &Path, mode: u32) -> Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    ensure!(
        f.metadata()?.is_file(),
        "not a regular target: {}",
        path.display()
    );
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detach_order_releases_children_and_data_alias_first() {
        let at = |p| PRIVATE_SOURCES.iter().position(|s| *s == p).unwrap();
        assert!(at("/sdcard") < at("/data"));
        assert!(at("/vendor/firmware/wlan/qca_cld") < at("/vendor/firmware"));
        assert!(!PRIVATE_SOURCES.contains(&"/tmp"));
    }
    #[test]
    fn private_detach_tolerates_absence_but_stops_at_busy_mount() {
        let mut calls = 0;
        detach_with(|_| {
            calls += 1;
            Err(io::Error::from_raw_os_error(if calls % 2 == 0 {
                libc::EINVAL
            } else {
                libc::ENOENT
            }))
        })
        .unwrap();
        assert_eq!(calls, 19);
        calls = 0;
        assert!(
            detach_with(|_| {
                calls += 1;
                Err(io::Error::from_raw_os_error(if calls == 3 {
                    libc::EBUSY
                } else {
                    libc::EINVAL
                }))
            })
            .is_err()
        );
        assert_eq!(calls, 3);
    }
    #[test]
    fn directory_mode_is_exact_and_symlinks_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("private");
        directory(&p, 0o700).unwrap();
        directory(p.join("root"), 0o755).unwrap();
        assert_eq!(fs::metadata(p.join("root")).unwrap().mode() & 0o777, 0o755);
        directory(p.join("root"), 0o700).unwrap();
        directory(p.join("root"), 0o755).unwrap();
        std::os::unix::fs::symlink(p.join("root"), tmp.path().join("link")).unwrap();
        assert!(directory(tmp.path().join("link"), 0o755).is_err());
        fs::write(tmp.path().join("file"), b"keep").unwrap();
        assert!(directory(tmp.path().join("file"), 0o755).is_err());
        assert_eq!(fs::read(tmp.path().join("file")).unwrap(), b"keep");
    }
}
