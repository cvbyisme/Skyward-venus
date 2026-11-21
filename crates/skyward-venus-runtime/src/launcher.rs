use crate::common::*;
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy)]
struct Role {
    program: &'static str,
    manager: bool,
    namespaced: bool,
    direct: bool,
}
fn role(name: &str) -> Result<Role> {
    let (program, manager, namespaced, direct) = match name {
        "hwmanager" => ("/system_ext/bin/hwservicemanager", true, false, false),
        "servicemanager" => ("/system/bin/servicemanager", true, false, true),
        "boot" => (
            "/vendor/bin/hw/android.hardware.boot-service.venus",
            false,
            true,
            true,
        ),
        "rmt_storage" => ("/vendor/bin/rmt_storage", false, true, true),
        "tftp" => ("/vendor/bin/tftp_server", false, true, true),
        "ipacm" => ("/vendor/bin/ipacm", false, true, true),
        "netmgr" => ("/vendor/bin/netmgrd", false, true, true),
        "network_suspend" => ("/skyward/runtime/network-suspend", false, true, true),
        "network_netd" => ("/skyward/runtime/network-netd", false, true, true),
        "netmgr_ready" => ("/skyward/runtime/netmgr-ready", false, true, true),
        "cnss" => ("/vendor/bin/cnss-daemon", false, true, true),
        "wpa" => ("/vendor/bin/hw/wpa_supplicant", false, true, true),
        "vndmanager" => ("/vendor/bin/vndservicemanager", false, true, true),
        "peripheral" => ("/vendor/bin/pm-service", false, true, true),
        "keymaster" => (
            "/vendor/bin/hw/android.hardware.keymaster@4.1-service-qti",
            false,
            false,
            false,
        ),
        "qseecomd" => ("/vendor/bin/qseecomd", false, false, false),
        "gatekeeper" => (
            "/vendor/bin/hw/android.hardware.gatekeeper@1.0-service-qti",
            false,
            false,
            false,
        ),
        _ => bail!("unsupported launcher role: {name}"),
    };
    Ok(Role {
        program,
        manager,
        namespaced,
        direct,
    })
}
struct Root(PathBuf);
impl Root {
    fn path(&self, s: &str) -> PathBuf {
        self.0.join(s.trim_start_matches('/'))
    }
    fn dir(&self, s: &str, mode: u32) -> Result<()> {
        directory(self.path(s), mode)
    }
    fn bind(
        &self,
        from: impl AsRef<Path>,
        to: &str,
        recursive: bool,
        readonly: bool,
    ) -> Result<()> {
        bind(from.as_ref(), &self.path(to), recursive, readonly)
    }
}
fn ram_workspace(path: &Path, gid: Option<u32>, forbidden: u32) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir()
            && m.uid() == 0
            && gid.is_none_or(|g| m.gid() == g)
            && m.mode() & forbidden == 0
            && fs_type(path)? == 0x01021994,
        "private RAM workspace: {}",
        path.display()
    );
    Ok(())
}
pub fn launch(args: &[OsString]) -> Result<()> {
    recovery()?;
    ensure!(
        args.len() == 1 || (args.len() == 2 && args[1] == "--list"),
        "usage: km41-launch ROLE [--list]"
    );
    let name = args[0].to_str().context("invalid role")?;
    let role = role(name)?;
    let listing = args.len() == 2;
    let cached = Path::new("/skyward/venus-recovery.txt").exists();
    if cached {
        cache_ready()?;
    }
    directory("/skyward/run/hal", 0o700)?;
    let root = Root(PathBuf::from(format!("/skyward/run/hal/root-{name}")));
    directory(&root.0, 0o755)?;
    private_mounts()?;
    for d in [
        "dev",
        "proc",
        "sys",
        "system",
        "system_ext",
        "vendor",
        "odm",
        "product",
        "apex",
        "linkerconfig",
        "tmp",
        "mnt",
        "data",
        "metadata",
        "firmware",
        "apex/com.android.runtime",
        "apex/com.android.vndk.v30",
    ] {
        root.dir(d, 0o755)?;
    }
    for (p, rec) in [("/dev", true), ("/proc", false), ("/sys", true)] {
        root.bind(p, p, rec, false)?;
    }
    let source = Path::new(if cached {
        "/skyward/run/hal/cache"
    } else {
        "/mnt/skyward-ro"
    });
    if name == "netmgr" {
        ensure!(cached, "NETMGR requires verified RAM runtime");
        for p in ["bin", "etc", "lib64"] {
            let dest = format!("system/{p}");
            root.dir(&dest, 0o755)?;
            root.bind(source.join("system/system").join(p), &dest, false, true)?;
        }
        let p = root.path("system/vendor");
        match fs::symlink_metadata(&p) {
            Ok(m) => ensure!(
                m.is_symlink() && fs::read_link(&p)? == Path::new("/vendor"),
                "private vendor alias changed"
            ),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::os::unix::fs::symlink("/vendor", p)?
            }
            Err(e) => return Err(e.into()),
        }
    } else {
        root.bind(source.join("system/system"), "system", false, true)?;
    }
    root.bind(source.join("system_ext"), "system_ext", false, true)?;
    if name == "tftp" {
        ensure!(cached, "TFTP requires verified RAM runtime");
        for p in ["bin", "etc", "lib64", "firmware", "rfs"] {
            let dest = format!("vendor/{p}");
            root.dir(&dest, 0o755)?;
            root.bind(source.join(&dest), &dest, false, true)?;
        }
        root.dir("vendor/firmware_mnt", 0o755)?;
        for p in [
            "mnt/vendor",
            "mnt/vendor/persist",
            "data/vendor",
            "data/vendor/tombstones",
            "data/vendor/tombstones/rfs",
        ] {
            root.dir(p, 0o755)?;
        }
        own(&root.path("data/vendor/tombstones/rfs"), 2903, 2903, 0o770)?;
        let scratch = Path::new("/skyward/run/hal/network-persist");
        ram_workspace(scratch, Some(2903), 0o027)?;
        root.bind(scratch, "mnt/vendor/persist", false, false)?;
    } else {
        root.bind(source.join("vendor"), "vendor", false, true)?;
    }
    for p in ["odm", "product"] {
        root.bind(source.join(p), p, false, true)?;
    }
    for (from, to) in [
        ("apex-runtime", "com.android.runtime"),
        ("apex-vndk30", "com.android.vndk.v30"),
    ] {
        root.bind(
            Path::new("/skyward/run/hal").join(from),
            &format!("apex/{to}"),
            false,
            true,
        )?;
    }
    if name == "netmgr" {
        root.dir("apex/com.android.tethering", 0o755)?;
        root.bind(
            "/skyward/run/hal/apex-tethering",
            "apex/com.android.tethering",
            false,
            true,
        )?;
    }
    let firmware = if cached {
        source.join("firmware")
    } else {
        PathBuf::from("/vendor/firmware_mnt")
    };
    for dest in ["vendor/firmware_mnt", "firmware"] {
        root.bind(&firmware, dest, false, true)?;
    }
    directory("/skyward/run/hal/scratch", 0o700)?;
    let scratch = PathBuf::from(format!("/skyward/run/hal/scratch/{name}"));
    directory(&scratch, 0o1777)?;
    root.bind(&scratch, "tmp", false, false)?;
    if name == "ipacm" {
        root.dir("data/vendor", 0o755)?;
        root.dir("data/vendor/ipa", 0o770)?;
        own(&root.path("data/vendor/ipa"), 1001, 1001, 0o770)?;
    }
    if name == "netmgr" {
        netmgr_paths(&root.0)?;
    }
    if matches!(name, "cnss" | "wpa") {
        root.dir("data/vendor", 0o755)?;
        for d in [
            "data/vendor/wifi",
            "data/vendor/wifi/sockets",
            "data/vendor/wifi/wpa",
            "data/vendor/wifi/wpa/sockets",
        ] {
            root.dir(d, 0o770)?;
            own(&root.path(d), 1010, 1010, 0o770)?;
        }
    }
    if name == "wpa" {
        for p in ["skyward", "skyward/run", "skyward/run/supplicant"] {
            root.dir(p, 0o755)?;
        }
        let home = Path::new("/skyward/run/supplicant");
        ram_workspace(home, None, 0o022)?;
        root.bind(home, "skyward/run/supplicant", false, false)?;
    }
    if matches!(name, "network_suspend" | "network_netd" | "netmgr_ready") {
        ensure!(cached, "network helper requires verified RAM runtime");
        let asset = network_asset(Path::new(role.program))?;
        let m = fs::symlink_metadata(&asset)?;
        ensure!(
            m.is_file() && m.uid() == 0 && m.mode() & 0o022 == 0 && m.mode() & 0o100 != 0,
            "network helper asset"
        );
        root.dir("skyward", 0o755)?;
        root.dir("skyward/runtime", 0o755)?;
        regular_target(&root.path(role.program), 0o700)?;
        root.bind(&asset, role.program, false, true)?;
        if name != "netmgr_ready" {
            directory("/skyward/run/locks", 0o700)?;
            ram_workspace(Path::new("/skyward/run/locks"), Some(0), 0o077)?;
            root.dir("skyward/run", 0o700)?;
            root.dir("skyward/run/locks", 0o700)?;
            root.bind("/skyward/run/locks", "skyward/run/locks", false, false)?;
        }
    }
    root.bind(
        "/skyward/run/hal/framework-manifest.xml",
        "system/etc/vintf/manifest.xml",
        false,
        true,
    )?;
    if role.namespaced {
        regular_target(&root.path("linkerconfig/ld.config.txt"), 0o600)?;
        root.bind(
            if name == "netmgr" {
                "/skyward/runtime/network-ld.config.txt"
            } else {
                "/skyward/runtime/boot-ld.config.txt"
            },
            "linkerconfig/ld.config.txt",
            false,
            true,
        )?;
    }
    if cached {
        detach_private_sources()?;
    }
    chroot(&root.0)?;
    if listing && matches!(name, "network_suspend" | "network_netd") {
        println!("{}: static Rust ELF; no shared libraries", role.program);
        return Ok(());
    }
    let loader = "/apex/com.android.runtime/bin/linker64";
    let mut command = Command::new(if listing || !role.direct {
        loader
    } else {
        role.program
    });
    command.env("PATH", "/system/bin:/vendor/bin:/system_ext/bin");
    for key in ["TMPDIR", "TMP", "TEMP"] {
        command.env(key, "/tmp");
    }
    if role.namespaced {
        command
            .env_remove("LD_LIBRARY_PATH")
            .env_remove("LD_PRELOAD");
    } else {
        command.env("LD_LIBRARY_PATH", if role.manager { "/apex/com.android.runtime/lib64/bionic:/system/lib64:/system_ext/lib64:/vendor/lib64:/apex/com.android.vndk.v30/lib64" } else { "/apex/com.android.runtime/lib64/bionic:/vendor/lib64:/apex/com.android.vndk.v30/lib64:/system/lib64" });
        if !role.manager {
            command.env("LD_PRELOAD", "/system/lib64/libc++.so");
        }
    }
    eprintln!(
        "KM41_LAUNCH role={name} root-only-diagnostic private-mount-namespace data-not-mounted"
    );
    if listing {
        command.args(["--list", role.program]);
    } else {
        if matches!(name, "cnss" | "vndmanager" | "peripheral") {
            helper_user(name == "cnss")?;
        }
        match name {
            "vndmanager" => {
                command.arg("/dev/vndbinder");
            }
            "cnss" => {
                command.args(["-n", "-l"]);
            }
            "wpa" => {
                command.args([
                    "-i",
                    "wlan0",
                    "-D",
                    "nl80211",
                    "-c",
                    "/skyward/run/supplicant/wpa_supplicant.conf",
                ]);
            }
            _ if !role.direct => {
                command.arg(role.program);
            }
            _ => {}
        }
    }
    exec(&mut command)
}
fn network_asset(program: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(program)?;
    if metadata.is_symlink() {
        ensure!(
            metadata.uid() == 0 && fs::read_link(program)? == Path::new("venus-runtime"),
            "foreign network helper alias"
        );
        Ok(program
            .parent()
            .context("helper parent")?
            .join("venus-runtime"))
    } else {
        Ok(program.to_owned())
    }
}
fn helper_user(cnss: bool) -> Result<()> {
    let groups: [libc::gid_t; 4] = [1000, 3003, 3005, 1010];
    cv(unsafe { libc::prctl(libc::PR_SET_KEEPCAPS, 1, 0, 0, 0) })?;
    cv(unsafe { libc::setgroups(if cnss { 4 } else { 1 }, groups.as_ptr()) })?;
    cv(unsafe { libc::setresgid(1000, 1000, 1000) })?;
    cv(unsafe { libc::setresuid(1000, 1000, 1000) })?;
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    let header = Header {
        version: 0x20080522,
        pid: 0,
    };
    let bit = if cnss { 1 << 12 } else { 0 };
    let data = [
        Data {
            effective: bit,
            permitted: bit,
            inheritable: bit,
        },
        Data {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
    ];
    cv(unsafe { libc::syscall(libc::SYS_capset, &header, data.as_ptr()) } as i32)?;
    if cnss {
        cv(unsafe { libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_RAISE, 12, 0, 0) })?;
    }
    Ok(())
}
fn ram_directory(f: &File) -> Result<fs::Metadata> {
    let m = f.metadata()?;
    let mut s = std::mem::MaybeUninit::<libc::statfs>::uninit();
    cv(unsafe { libc::fstatfs(f.as_raw_fd(), s.as_mut_ptr()) })?;
    ensure!(
        m.is_dir() && unsafe { s.assume_init() }.f_type == 0x01021994,
        "NETMGR requires RAM directory"
    );
    Ok(m)
}
fn root_directory(path: &Path) -> Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let m = ram_directory(&f)?;
    ensure!(
        m.uid() == 0 && m.gid() == 0 && m.mode() & 0o7777 == 0o755,
        "foreign NETMGR root directory"
    );
    Ok(f)
}
fn directory_at(
    parent: &File,
    name: &str,
    uid: u32,
    gid: u32,
    mode: u32,
    create: bool,
) -> Result<File> {
    let before = ram_directory(parent)?;
    let name = c(name)?;
    let made = if create {
        match cv(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) }) {
            Ok(()) => true,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e.into()),
        }
    } else {
        false
    };
    let raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    cv(raw)?;
    let f = unsafe { File::from_raw_fd(raw) };
    let m = ram_directory(&f)?;
    ensure!(m.dev() == before.dev(), "foreign NETMGR directory mount");
    if made {
        cv(unsafe { libc::fchown(raw, uid, gid) })?;
        cv(unsafe { libc::fchmod(raw, mode as _) })?;
    }
    let m = f.metadata()?;
    ensure!(
        m.uid() == uid && m.gid() == gid && m.mode() & 0o7777 == mode,
        "foreign NETMGR directory ownership or mode"
    );
    Ok(f)
}
fn recovery_file(parent: &File, uid: u32, gid: u32) -> Result<()> {
    let before = ram_directory(parent)?;
    let mut made = true;
    let mut raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            c"recovery_info".as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if raw < 0 && io::Error::last_os_error().kind() == io::ErrorKind::AlreadyExists {
        made = false;
        raw = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                c"recovery_info".as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
    }
    cv(raw)?;
    let f = unsafe { File::from_raw_fd(raw) };
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.nlink() == 1 && m.dev() == before.dev(),
        "foreign NETMGR recovery_info"
    );
    if made {
        cv(unsafe { libc::fchown(raw, uid, gid) })?;
        cv(unsafe { libc::fchmod(raw, 0o600) })?;
    }
    let m = f.metadata()?;
    ensure!(
        m.uid() == uid && m.gid() == gid && m.mode() & 0o600 == 0o600 && m.mode() & 0o7133 == 0,
        "foreign NETMGR recovery_info ownership or mode"
    );
    Ok(())
}
fn netmgr_paths(root: &Path) -> Result<()> {
    let dev = root_directory(Path::new("/dev"))?;
    let sockets = directory_at(&dev, "socket", 0, 0, 0o755, false)?;
    directory_at(&sockets, "netmgr", 1001, 1001, 0o750, true)?;
    let home = root_directory(root)?;
    let data = directory_at(&home, "data", 0, 0, 0o755, false)?;
    let vendor = directory_at(&data, "vendor", 0, 0, 0o755, true)?;
    let state = directory_at(&vendor, "netmgr", 1001, 1001, 0o770, true)?;
    let recovery = directory_at(&state, "recovery", 1001, 1001, 0o770, true)?;
    recovery_file(&recovery, 1001, 1001)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn role_mapping_keeps_platform_and_vendor_abis_separate() {
        assert!(role("hwmanager").unwrap().manager);
        assert!(!role("keymaster").unwrap().namespaced);
        assert!(role("netmgr").unwrap().namespaced);
        assert!(role("servicemanager").unwrap().direct);
        assert!(role("../vendor").is_err());
    }
    #[test]
    fn ram_state_preserves_bytes_and_rejects_foreign_inodes() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let tmp = tempfile::tempdir_in("/dev/shm").unwrap();
        let base = File::open(tmp.path()).unwrap();
        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };
        let socket = directory_at(&base, "netmgr", uid, gid, 0o750, true).unwrap();
        directory_at(&base, "netmgr", uid, gid, 0o750, true).unwrap();
        assert!(directory_at(&base, "netmgr", uid + 1, gid, 0o750, true).is_err());
        socket
            .set_permissions(fs::Permissions::from_mode(0o777))
            .unwrap();
        assert!(directory_at(&base, "netmgr", uid, gid, 0o750, true).is_err());
        assert_eq!(socket.metadata().unwrap().mode() & 0o777, 0o777);
        socket
            .set_permissions(fs::Permissions::from_mode(0o750))
            .unwrap();
        symlink("netmgr", tmp.path().join("alias")).unwrap();
        assert!(directory_at(&base, "alias", uid, gid, 0o750, true).is_err());
        fs::write(tmp.path().join("plain"), b"original").unwrap();
        assert!(directory_at(&base, "plain", uid, gid, 0o750, true).is_err());
        recovery_file(&socket, uid, gid).unwrap();
        let path = tmp.path().join("netmgr/recovery_info");
        fs::write(&path, b"previous\n").unwrap();
        recovery_file(&socket, uid, gid).unwrap();
        assert!(recovery_file(&socket, uid + 1, gid).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"previous\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(recovery_file(&socket, uid, gid).is_err());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o666);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = tmp.path().join("hardlink");
        fs::hard_link(&path, &link).unwrap();
        assert!(recovery_file(&socket, uid, gid).is_err());
        fs::remove_file(link).unwrap();
        let saved = tmp.path().join("saved");
        fs::rename(&path, &saved).unwrap();
        symlink(&saved, &path).unwrap();
        assert!(recovery_file(&socket, uid, gid).is_err());
        fs::remove_file(&path).unwrap();
        cv(unsafe { libc::mkfifo(c(&path).unwrap().as_ptr(), 0o600) }).unwrap();
        assert!(recovery_file(&socket, uid, gid).is_err());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(recovery_file(&socket, uid, gid).is_err());
        assert_eq!(fs::read(saved).unwrap(), b"previous\n");
    }
    #[test]
    fn non_ram_parent_is_rejected_before_creation() {
        let tmp = tempfile::tempdir().unwrap();
        if fs_type(tmp.path()).unwrap() == 0x01021994 {
            return;
        }
        let parent = File::open(tmp.path()).unwrap();
        assert!(directory_at(&parent, "must-not-create", 0, 0, 0o750, true).is_err());
        assert!(!tmp.path().join("must-not-create").exists());
    }
}
