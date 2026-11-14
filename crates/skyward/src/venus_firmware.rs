//! Publish the two distinct installed firmware trees from the verified RAM
//! cache. Stock recovery overlays modem_a/image on /vendor/firmware, hiding
//! vendor's IPA firmware; the modem still needs /firmware/image separately.
use anyhow::{ensure, Context, Result};
use procfs::process::Process;
use skyward_block::mount;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub fn cached_bind(at: &Path, source: &Path) -> Result<bool> {
    let Some(shown) = mount::visible(at)? else { return Ok(false) };
    if shown.filesystem != "tmpfs" { return Ok(false); }
    let expected = fs::metadata(source)?;
    let actual = fs::metadata(at)?;
    if (expected.dev(),expected.ino()) != (actual.dev(),actual.ino()) { return Ok(false); }
    let mounts = Process::myself()?.mountinfo()?.0;
    let visible = mounts.iter().rev().find(|m| m.mount_point == at).context("firmware mount disappeared")?;
    ensure!(visible.mount_options.contains_key("ro"), "RAM firmware bind must be read-only");
    Ok(true)
}

pub fn publish() -> Result<()> {
    ensure!(crate::venus_rw::enabled() && unsafe { libc::geteuid() } == 0, "marked Venus Recovery and root required");
    let cache = crate::venus_rw::cache_root()?.context("verified RAM runtime is not ready")?;
    let slot = skyward_vab::current_slot()?.context("current A/B slot unavailable")?;
    ensure!(skyward_vab::snapshot_state()? == skyward_vab::SnapshotState::None, "OTA snapshot is outstanding");
    let device = skyward_block::devnum(Path::new("/dev/block/by-name").join(format!("modem{}",slot.suffix())))?;
    let targets = [
        // /etc is the ramdisk's /system/etc symlink. Mountinfo records
        // its resolved target, which ownership checks must also use.
        ("/system/etc/firmware","vendor/firmware"),
        ("/firmware","firmware"),
        ("/vendor/firmware","vendor/firmware"),
        ("/vendor/etc/wifi","vendor/etc/wifi"),
    ];
    // Check the entire plan before replacing the stock firmware alias.
    for (target,relative) in targets {
        let (at,source) = (Path::new(target),cache.join(relative));
        ensure!(fs::symlink_metadata(&source)?.is_dir(), "firmware source must be a real directory");
        if let Ok(meta) = fs::symlink_metadata(at) {
            ensure!(meta.is_dir(), "firmware target must be a real directory: {target}");
        }
        if cached_bind(at,&source)? { continue; }
        if let Some(shown) = mount::visible(at)? {
            ensure!(target == "/vendor/firmware" && shown.device == device && shown.filesystem == "vfat", "foreign firmware mount: {target}");
            let mounts = Process::myself()?.mountinfo()?.0;
            let visible = mounts.iter().rev().find(|m| m.mount_point == at).context("stock firmware alias disappeared")?;
            ensure!(visible.root == "/image" && visible.mount_options.contains_key("ro"), "unexpected stock modem firmware alias");
        }
    }
    for (target,relative) in targets {
        let (at,source) = (Path::new(target),cache.join(relative));
        if cached_bind(at,&source)? { continue; }
        if mount::is_mount_point(at)? { mount::umount(at)?; }
        fs::create_dir_all(at)?;
        mount::bind(&source,at)?;
        mount::mount(&source,at,"none",&["remount".into(),"bind".into(),"ro".into(),"nosuid".into(),"nodev".into()])?;
        ensure!(cached_bind(at,&source)?, "firmware RAM bind verification failed: {target}");
    }
    eprintln!("VENUS_FIRMWARE_READY vendor_and_modem_trees=separate verified_ram=true original_partitions_written=false");
    Ok(())
}
