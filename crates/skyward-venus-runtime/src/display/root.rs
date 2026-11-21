//! Private display experiment roots. Every source inode is pinned before a
//! non-recursive, read-only bind; no whole Data/proc/sys tree reaches plugins.
use crate::common::{c, cv, fs_type, mount, private_mounts};
use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Path, PathBuf},
    process::Command,
};
const CONFIG: &str = include_str!("../../../../porting/venus/boot-ld.config.txt");
pub fn close_extra_fds() -> Result<()> {
    let fds = fs::read_dir("/proc/self/fd")?
        .map(|entry| -> Result<Option<i32>> {
            Ok(entry?
                .file_name()
                .to_str()
                .and_then(|s| s.parse().ok())
                .filter(|fd| *fd > 2))
        })
        .collect::<Result<Vec<_>>>()?;
    for fd in fds.into_iter().flatten() {
        unsafe {
            libc::close(fd);
        }
    }
    Ok(())
}
pub fn flags(path: &Path) -> Result<libc::c_ulong> {
    let path = c(path)?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    cv(unsafe { libc::statvfs(path.as_ptr(), st.as_mut_ptr()) })?;
    Ok(unsafe { st.assume_init() }.f_flag)
}
fn unescape(field: &str) -> String {
    field
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}
fn persist_options(path: &Path, mounts: &str) -> Result<()> {
    let mut best = 0;
    let mut safe = false;
    for line in mounts.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let point = PathBuf::from(unescape(fields[1]));
        let length = point.as_os_str().as_bytes().len();
        if length >= best && path.starts_with(&point) {
            best = length;
            let options: Vec<_> = fields[3].split(',').collect();
            safe = fields[2] == "ext4"
                && options.contains(&"ro")
                && (options.contains(&"noload") || options.contains(&"norecovery"));
        }
    }
    ensure!(
        best > 0 && safe,
        "persist must be on ext4 mounted ro,noload or norecovery"
    );
    Ok(())
}
pub fn persist_checked(path: &Path) -> Result<()> {
    let actual = fs::canonicalize(path)?;
    ensure!(
        flags(&actual)? & libc::ST_RDONLY != 0,
        "persist source must already be read-only"
    );
    persist_options(&actual, &fs::read_to_string("/proc/mounts")?)
}
pub fn temp_directory(pattern: &str) -> Result<PathBuf> {
    let mut pattern = c(pattern)?.into_bytes_with_nul();
    let pointer = unsafe { libc::mkdtemp(pattern.as_mut_ptr().cast()) };
    ensure!(
        !pointer.is_null(),
        "private directory: {}",
        std::io::Error::last_os_error()
    );
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(
        &pattern[..pattern.len() - 1],
    )))
}
pub struct Root(pub PathBuf);
impl Root {
    pub fn new(kind: &str, ipc: bool) -> Result<Self> {
        ensure!(
            fs_type("/tmp")? == 0x01021994,
            "display scratch must be RAM tmpfs"
        );
        private_mounts()?;
        let root = Self(temp_directory(&format!(
            "/skyward/run/tmp/venus-display-{kind}-XXXXXX"
        ))?);
        mount(
            Some(Path::new("display-experiment")),
            &root.0,
            Some("tmpfs"),
            libc::MS_NOSUID | libc::MS_NODEV,
            Some("mode=0755,size=2m"),
        )?;
        for dir in [
            "system",
            "system_ext",
            "vendor",
            "odm",
            "apex",
            "apex/com.android.runtime",
            "apex/com.android.vndk.v30",
            "linkerconfig",
            "dev",
            "dev/__properties__",
            "tmp",
            "skyward",
            "skyward/runtime",
            "data",
            "data/vendor",
            "data/vendor/display",
            "mnt",
            "mnt/vendor",
            "mnt/vendor/persist",
            "mnt/vendor/persist/display",
            "persist",
            "persist/display",
        ] {
            root.directory(dir)?;
        }
        if ipc {
            for dir in ["run", ".binder", "proc", "sys", "sys/fs", "sys/fs/selinux"] {
                root.directory(dir)?;
            }
            root.file("run/owner.sock", b"")?;
        }
        root.file("dev/vndbinder", b"")?;
        root.file("data/vendor/display/DF_Param.xml", b"")?;
        root.file("linkerconfig/ld.config.txt", CONFIG.as_bytes())?;
        for (source, dest, already_ro) in [
            ("/mnt/skyward-ro/system/system", "system", true),
            ("/mnt/skyward-ro/system_ext", "system_ext", true),
            ("/mnt/skyward-ro/vendor", "vendor", true),
            (
                "/skyward/run/hal/apex-runtime",
                "apex/com.android.runtime",
                false,
            ),
            (
                "/skyward/run/hal/apex-vndk30",
                "apex/com.android.vndk.v30",
                false,
            ),
            ("/dev/__properties__", "dev/__properties__", false),
        ] {
            root.bind(Path::new(source), dest, true, already_ro)?;
        }
        Ok(root)
    }
    pub fn path(&self, path: &str) -> PathBuf {
        self.0.join(path.trim_start_matches('/'))
    }
    pub fn directory(&self, path: &str) -> Result<()> {
        let path = self.path(path);
        fs::create_dir(&path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        Ok(())
    }
    pub fn file(&self, path: &str, contents: &[u8]) -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.path(path))?;
        file.write_all(contents)?;
        Ok(())
    }
    pub fn bind(&self, source: &Path, dest: &str, directory: bool, already_ro: bool) -> Result<()> {
        ensure!(source.is_absolute(), "source must be absolute");
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(source)?;
        let metadata = file.metadata()?;
        ensure!(
            if directory {
                metadata.is_dir()
            } else {
                metadata.is_file()
            },
            "unexpected source inode type: {}",
            source.display()
        );
        let pinned = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        if already_ro {
            ensure!(
                flags(&pinned)? & libc::ST_RDONLY != 0,
                "original filesystem must already be read-only"
            );
        }
        let dest = self.path(dest);
        mount(Some(&pinned), &dest, None, libc::MS_BIND, None)?;
        mount(
            None,
            &dest,
            None,
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV,
            None,
        )?;
        ensure!(
            flags(&dest)? & libc::ST_RDONLY != 0,
            "read-only bind verification"
        );
        Ok(())
    }
    pub fn real_binder(&self) -> Result<()> {
        let actual = fs::canonicalize("/dev/vndbinder")?;
        ensure!(
            [
                Path::new("/dev/vndbinder"),
                Path::new("/dev/binderfs/vndbinder")
            ]
            .contains(&actual.as_path()),
            "unexpected vendor Binder location"
        );
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&actual)?;
        let before = file.metadata()?;
        ensure!(
            before.file_type().is_char_device(),
            "vendor Binder must be a character device"
        );
        self.binder_bind(Path::new(&format!("/proc/self/fd/{}", file.as_raw_fd())))?;
        let dest = self.path("dev/vndbinder");
        let after = fs::metadata(&dest)?;
        let mount_flags = flags(&dest)?;
        ensure!(
            mount_flags & libc::ST_RDONLY != 0
                && mount_flags & libc::ST_NODEV == 0
                && after.file_type().is_char_device()
                && before.rdev() == after.rdev(),
            "vendor Binder bind verification"
        );
        Ok(())
    }
    fn binder_bind(&self, source: &Path) -> Result<()> {
        let dest = self.path("dev/vndbinder");
        mount(Some(source), &dest, None, libc::MS_BIND, None)?;
        mount(
            None,
            &dest,
            None,
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NOEXEC,
            None,
        )
    }
    pub fn private_binder(&self) -> Result<()> {
        let base = self.path(".binder");
        mount(
            Some(Path::new("binder")),
            &base,
            Some("binder"),
            libc::MS_NOSUID | libc::MS_NOEXEC,
            Some("max=8"),
        )?;
        crate::binder::node(&base, "skyward-display", 0o600)?;
        let node = base.join("skyward-display");
        let private = fs::metadata(&node)?;
        let global = fs::metadata("/dev/vndbinder")?;
        ensure!(
            private.file_type().is_char_device() && private.rdev() != global.rdev(),
            "private Binder must differ from global driver"
        );
        self.binder_bind(&node)?;
        cv(unsafe { libc::umount(c(&base)?.as_ptr()) })?;
        eprintln!(
            "DISPLAY_IPC_BINDER private_rdev={} global_registry=untouched",
            private.rdev()
        );
        Ok(())
    }
    pub fn socket_bind(&self, source: &Path) -> Result<()> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(source)?;
        let m = file.metadata()?;
        ensure!(
            m.file_type().is_socket() && m.uid() == 0 && m.mode() & 0o077 == 0,
            "owner socket inode/mode"
        );
        let dest = self.path("run/owner.sock");
        let pinned = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        mount(Some(&pinned), &dest, None, libc::MS_BIND, None)?;
        mount(
            None,
            &dest,
            None,
            libc::MS_BIND
                | libc::MS_REMOUNT
                | libc::MS_RDONLY
                | libc::MS_NOSUID
                | libc::MS_NODEV
                | libc::MS_NOEXEC,
            None,
        )
    }
    pub fn seal(&self) -> Result<()> {
        mount(
            None,
            &self.0,
            None,
            libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV,
            None,
        )
    }
    pub fn calibration(&self, param: &Path, persist: Option<&Path>) -> Result<()> {
        self.bind(param, "data/vendor/display/DF_Param.xml", false, false)?;
        if let Some(persist) = persist {
            for dest in ["mnt/vendor/persist/display", "persist/display"] {
                self.bind(persist, dest, true, true)?;
            }
        }
        Ok(())
    }
}
pub fn probe_launch(args: &[OsString]) -> Result<()> {
    ensure!(
        args.len() >= 4,
        "usage: display-probe-launch PROBE DF_PARAM --uncalibrated|--persist-display DIR --list|--render INDEX QDCM_ID"
    );
    crate::common::root()?;
    let (persist, first) = if args[2] == "--persist-display" {
        ensure!(args.len() >= 5, "missing persist path");
        (Some(Path::new(&args[3])), 4)
    } else {
        ensure!(args[2] == "--uncalibrated", "calibration mode required");
        (None, 3)
    };
    let mode = &args[first..];
    ensure!(
        (mode.len() == 1 && mode[0] == "--list") || (mode.len() == 3 && mode[0] == "--render"),
        "invalid display probe mode"
    );
    for number in &mode[1..] {
        let value: u32 = number.to_str().context("mode number UTF8")?.parse()?;
        ensure!(value <= i32::MAX as u32, "invalid mode number");
    }
    close_extra_fds()?;
    if let Some(path) = persist {
        persist_checked(path)?;
    }
    let root = Root::new("probe", false)?;
    root.file("skyward/runtime/display-probe", b"")?;
    root.bind(
        Path::new(&args[0]),
        "skyward/runtime/display-probe",
        false,
        false,
    )?;
    root.real_binder()?;
    root.calibration(Path::new(&args[1]), persist)?;
    root.seal()?;
    eprintln!(
        "DISPLAY_PROBE_LAUNCH root={} proc=absent data=DF_Param-only vndbinder=real-preinit-only persist={}; remove empty root directory after exit",
        root.0.display(),
        if persist.is_some() {
            "read-only-no-replay"
        } else {
            "ABSENT-UNCALIBRATED"
        }
    );
    crate::common::chroot(&root.0)?;
    crate::common::exec(
        Command::new("/skyward/runtime/display-probe")
            .args(mode)
            .env_clear()
            .env("PATH", "/system/bin"),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persist_requires_most_specific_readonly_no_replay_mount() {
        let safe =
            "/dev/root / ext4 ro,noload 0 0\n/dev/persist /mnt/persist ext4 ro,norecovery 0 0\n";
        assert!(persist_options(Path::new("/mnt/persist/display"), safe).is_ok());
        for extra in [
            "/dev/other /mnt/persist/display ext4 rw,noload 0 0",
            "/dev/other /mnt/persist/display ext4 ro 0 0",
            "tmpfs /mnt/persist/display tmpfs ro 0 0",
        ] {
            assert!(
                persist_options(
                    Path::new("/mnt/persist/display"),
                    &format!("{safe}{extra}\n")
                )
                .is_err()
            );
        }
        assert!(
            persist_options(
                Path::new("/mnt/persist/display"),
                "/dev/p /mnt/persist2 ext4 ro,noload 0 0"
            )
            .is_err()
        );
    }
}
