//! Release Venus's source mounts only after the live HALs have been shown
//! to use the verified RAM copy. No service restart or lazy unmount.
use anyhow::{ensure, Context, Result};
use procfs::process::Process;
use skyward_block::{dm, mount};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const RUNTIME: &str = "/tmp/skyward-km41";
const ROLES: &[&str] = &["hwmanager", "qseecomd", "keymaster", "gatekeeper", "servicemanager", "boot"];

fn pid(text: &str) -> Result<i32> {
    let text = text.trim();
    ensure!(!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()), "invalid runtime PID");
    let pid = text.parse()?;
    ensure!(pid > 1, "invalid runtime PID");
    Ok(pid)
}

/// Process start times prevent a recycled PID from satisfying the final
/// check after the source mounts have been released.
#[derive(PartialEq, Eq)]
pub struct CachedRuntime(Vec<(i32, u64)>);

pub fn verified() -> Result<CachedRuntime> {
    ensure!(crate::venus_rw::enabled() && unsafe { libc::geteuid() } == 0, "marked Venus Recovery and root required");
    let cache = crate::venus_rw::cache_root()?.context("verified RAM runtime is not ready")?;
    ensure!(skyward_sys::prop("init.svc.venus_security").as_deref() == Some("running"), "security owner is not running");
    ensure!(fs::read_to_string("/tmp/skyward-recovery/runtime-ready")? == "venus-runtime-ready-v2\n", "security startup is incomplete or predates full firmware-source release");
    for (name, value) in [
        ("hwservicemanager.ready", "true"),
        ("vendor.sys.listeners.registered", "true"),
        ("skyward.binder.domain", "system"),
    ] {
        ensure!(skyward_sys::prop(name).as_deref() == Some(value), "runtime readiness changed: {name}");
    }
    let owner = pid(&skyward_sys::prop("init.svc_debug_pid.venus_security").context("security owner PID unavailable")?)?;
    let owner_group = fs::read_to_string(format!("/proc/{owner}/cgroup"))?;
    let our_namespace = fs::read_link("/proc/self/ns/mnt")?;
    let mut namespaces = BTreeSet::new();
    let mut processes = Vec::new();
    for role in ROLES {
        let id = pid(&fs::read_to_string(Path::new(RUNTIME).join(format!("{role}.pid")))?)?;
        let proc = PathBuf::from(format!("/proc/{id}"));
        ensure!(fs::read_link(proc.join("root"))? == Path::new(RUNTIME).join(format!("root-{role}")), "foreign runtime root: {role}");
        ensure!(fs::read_to_string(proc.join("cgroup"))? == owner_group, "runtime lost its independent owner: {role}");
        let ns = fs::read_link(proc.join("ns/mnt"))?;
        ensure!(ns != our_namespace && namespaces.insert(ns), "runtime mount namespace is not private: {role}");
        let process = Process::new(id)?;
        let stat = process.stat()?;
        ensure!(stat.state != 'Z' && stat.state != 'X', "runtime process exited: {role}");
        let mounts = process.mountinfo()?.0;
        for (target, relative) in [
            ("/system", "system/system"), ("/system_ext", "system_ext"),
            ("/vendor", "vendor"), ("/odm", "odm"), ("/product", "product"),
            ("/vendor/firmware_mnt", "firmware"), ("/firmware", "firmware"),
        ] {
            let shown = mounts.iter().rev().find(|m| m.mount_point == Path::new(target))
                .with_context(|| format!("runtime mount missing: {role} {target}"))?;
            ensure!(shown.fs_type == "tmpfs" && shown.mount_options.contains_key("ro"), "runtime still uses a writable or non-RAM source: {role} {target}");
            let expected = fs::metadata(cache.join(relative))?;
            let actual = fs::metadata(proc.join("root").join(target.trim_start_matches('/')))?;
            ensure!((actual.dev(), actual.ino()) == (expected.dev(), expected.ino()), "runtime uses a different RAM tree: {role} {target}");
        }
        processes.push((id, stat.starttime));
    }
    Ok(CachedRuntime(processes))
}

impl CachedRuntime {
    fn unchanged(&self) -> Result<()> {
        ensure!(*self == verified()?, "security runtime changed during source release");
        Ok(())
    }
}

fn lock() -> Result<File> {
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/tmp/skyward-sources.lock")?;
    ensure!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0, "another source mount operation is running");
    Ok(file)
}

pub fn unmount_one(base: &str) -> Result<()> {
    let _lock = lock()?;
    let before = verified()?;
    crate::venus_basic::runtime::unmount_one(base)?;
    before.unchanged()
}

pub fn release_sources() -> Result<()> {
    let _radio_storage = crate::venus_modem::efs_guard()?;
    let _lock = lock()?;
    let before = verified()?;
    let slot = skyward_vab::current_slot()?.context("A/B slot unavailable")?;
    ensure!(skyward_vab::snapshot_state()? == skyward_vab::SnapshotState::None, "an update snapshot is outstanding");
    // This adapter owns only the explicitly isolated venus_ro_* maps.
    // A generic/foreign map must not be silently adopted or lazily detached.
    for base in skyward_vab::slot_bases(slot)? {
        ensure!(dm::status(&format!("{base}{}", slot.suffix()))?.is_none(), "another mapping already owns {base}");
    }
    let cache = crate::venus_rw::cache_root()?.context("RAM runtime disappeared")?;
    let mut firmware_sources = Vec::new();
    for (target,relative) in [("/vendor/firmware","vendor/firmware"),("/vendor/firmware_mnt","firmware")] {
        let firmware = Path::new(target);
        if crate::venus_firmware::cached_bind(firmware,&cache.join(relative))? { continue; }
        let Some(shown) = mount::visible(firmware)? else { continue };
        let device = Path::new("/dev/block/by-name").join(format!("modem{}", slot.suffix()));
        ensure!(shown.device == skyward_block::devnum(&device)? && shown.filesystem == "vfat", "foreign modem firmware mount: {target}");
        let mounts = Process::myself()?.mountinfo()?.0;
        let shown = mounts.iter().rev().find(|m| m.mount_point == firmware).context("firmware mount disappeared")?;
        ensure!(shown.mount_options.contains_key("ro"), "modem firmware must be read-only");
        firmware_sources.push(firmware.to_owned());
    }
    for base in crate::venus_basic::PARTITIONS.iter().rev() {
        crate::venus_basic::runtime::unmount_one(base)?;
    }
    for firmware in firmware_sources { mount::umount(&firmware)?; }
    before.unchanged()?;
    eprintln!("VENUS_RUNTIME_SOURCES_RELEASED services_unchanged=true lazy_unmount=false");
    Ok(())
}
