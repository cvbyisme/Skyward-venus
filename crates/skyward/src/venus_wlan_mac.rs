//! Adapt this phone's installed factory WLAN address to its driver's
//! firmware-file format. Never synthesize addresses or write persist.
use anyhow::{ensure, Context, Result};

/// Publish only a fully prepared directory. Each attempt owns its staging
/// directory; a failed copy/write must neither poison retries nor remove
/// another attempt's files.
fn publish_staged(root: &std::path::Path, build: impl FnOnce(&std::path::Path) -> Result<()>) -> Result<()> {
    use std::fs::{self, DirBuilder};
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Staging(std::path::PathBuf);
    impl Drop for Staging {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_dir_all(&self.0) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("release WLAN firmware staging: {error}");
                }
            }
        }
    }
    let parent = root.parent().context("WLAN firmware parent unavailable")?;
    let name = root.file_name().context("WLAN firmware name unavailable")?.to_string_lossy();
    let mut stage = None;
    for _ in 0..64 {
        let path = parent.join(format!(".{name}.new.{}.{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => { stage = Some(Staging(path)); break; }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("create WLAN firmware staging"),
        }
    }
    let stage = stage.context("WLAN firmware staging names exhausted")?;
    build(&stage.0)?;
    fs::rename(&stage.0, root).context("publish WLAN firmware")?;
    Ok(())
}

fn driver_config(factory: &[u8]) -> Result<Vec<u8>> {
    let line = factory.strip_suffix(b"\r\n").or_else(|| factory.strip_suffix(b"\n")).unwrap_or(factory);
    let hex = line.strip_prefix(b"wlan0=").context("unrecognized Venus factory WLAN address format")?;
    ensure!(hex.len() == 12 && hex.iter().all(u8::is_ascii_hexdigit), "invalid Venus factory WLAN address");
    let value = std::str::from_utf8(hex)?;
    let mut mac = [0u8;6];
    for (i,out) in mac.iter_mut().enumerate() { *out = u8::from_str_radix(&value[i*2..i*2+2],16)?; }
    ensure!(mac[0]&1 == 0 && mac != [0;6] && mac != [255;6], "factory WLAN address is not a unicast address");
    // Exact parser documented in this Venus kernel's wlan_hdd_cfg.c.
    Ok(format!("Intf0MacAddress={}\nEND\n",value.to_ascii_uppercase()).into_bytes())
}

#[cfg(target_os = "android")]
mod runtime {
    use super::*;
    use sha2::{Digest, Sha256};
    use skyward_block::mount;
    use std::fs::{self, DirBuilder, OpenOptions};
    use std::io::{Read, Write};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::Path;
    use std::process::Command;

    fn identity(path: &Path) -> Result<(u64,u64)> {
        let m=fs::symlink_metadata(path)?;
        ensure!(m.is_dir() && !m.file_type().is_symlink(), "WLAN firmware directory must be a real directory");
        Ok((m.dev(),m.ino()))
    }

    fn factory_config() -> Result<Vec<u8>> {
        let at=Path::new("/skyward/run/hal/wlan-persist");
        if !at.exists() { DirBuilder::new().mode(0o700).create(at)?; }
        let m=fs::symlink_metadata(at)?;
        ensure!(m.is_dir() && !m.file_type().is_symlink() && m.uid()==0 && m.mode()&0o077==0,
            "foreign WLAN persist mount directory");
        ensure!(!mount::is_mount_point(at)?, "WLAN persist source is already mounted");
        let device=Path::new("/dev/block/by-name/persist");
        let expected=skyward_block::devnum(device)?;
        mount::mount(device,at,"ext4",&["ro".into(),"noload".into(),"nosuid".into(),"nodev".into(),"noexec".into()])?;
        let read=(|| -> Result<Vec<u8>> {
            let shown=mount::visible(at)?.context("WLAN persist source mount disappeared")?;
            ensure!(shown.device==expected && shown.filesystem=="ext4", "foreign WLAN persist source");
            let path=at.join("wlan_mac.bin");
            let mut file=OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(path)?;
            let meta=file.metadata()?;
            ensure!(meta.is_file() && meta.len()<=64, "invalid factory WLAN address file");
            let mut bytes=Vec::new(); file.read_to_end(&mut bytes)?;
            driver_config(&bytes)
        })();
        // Even a malformed/missing factory file must let go of persist.
        mount::umount(at).context("release read-only WLAN persist source")?;
        read
    }

    pub fn prepare() -> Result<()> {
        crate::venus_runtime::verified()?;
        let cache=crate::venus_rw::cache_root()?.context("WLAN RAM firmware is not ready")?;
        let original=cache.join("vendor/firmware/wlan/qca_cld");
        let original_id=identity(&original)?;
        // These two global aliases inherited /tmp's shared propagation.
        // Isolate each parent before adding a child mount, so the factory
        // file cannot propagate into the verified cache or the other alias.
        for parent in ["/system/etc/firmware","/vendor/firmware"] {
            let at=Path::new(parent);
            ensure!(crate::venus_firmware::cached_bind(at,&cache.join("vendor/firmware"))?, "foreign WLAN firmware parent");
            mount::mount(Path::new("none"),at,"none",&["private".into(),"rec".into()])?;
        }
        let config=factory_config()?;
        let boot=fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        let receipt=format!("venus-wlan-factory-mac-v1\nboot={}\nconfig_sha256={:x}\n",boot.trim(),Sha256::digest(&config));
        let root=Path::new("/skyward/run/hal/wlan-firmware");
        if root.exists() {
            ensure!(fs::read(root.join(".skyward-ready"))?==receipt.as_bytes(), "foreign WLAN firmware receipt");
            ensure!(fs::read(root.join("wlan_mac.bin"))?==config, "WLAN firmware address changed");
        } else {
            publish_staged(root, |stage| {
                // All existing firmware keeps its bytes and metadata, without
                // another large RAM copy. Never alter the verified cache tree.
                let status=Command::new("/system/bin/cp").args(["-a","-l"])
                    .arg(original.join(".")).arg(stage).status()?;
                ensure!(status.success(), "mirror verified WLAN firmware");
                let file=stage.join("wlan_mac.bin");
                ensure!(!file.exists() && fs::symlink_metadata(&file).is_err(), "installed WLAN MAC firmware already exists; inspect its format first");
                let mut out=OpenOptions::new().write(true).create_new(true).mode(0o644)
                    .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(file)?;
                out.write_all(&config)?;out.sync_all()?;drop(out);
                fs::set_permissions(stage.join("wlan_mac.bin"),fs::Permissions::from_mode(0o644))?;
                let mut marker=OpenOptions::new().write(true).create_new(true).mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(stage.join(".skyward-ready"))?;
                marker.write_all(receipt.as_bytes())?;marker.sync_all()?;drop(marker);
                Ok(())
            })?;
        }
        let owned_id=identity(root)?;
        for target in ["/system/etc/firmware/wlan/qca_cld","/vendor/firmware/wlan/qca_cld"] {
            let at=Path::new(target);
            let id=identity(at)?;
            ensure!(id==original_id || id==owned_id, "foreign WLAN firmware mount");
            if id!=owned_id {
                mount::bind(root,at)?;
                let protect=|| -> Result<()> {
                    mount::mount(Path::new("none"),at,"none",&["private".into(),"rec".into()])?;
                    mount::mount(root,at,"none",&["remount".into(),"bind".into(),"ro".into(),"nosuid".into(),"nodev".into(),"noexec".into()])
                };
                if let Err(error)=protect() {
                    mount::umount(at)?;
                    return Err(error).context("make factory WLAN firmware read-only");
                }
            }
            ensure!(crate::venus_firmware::cached_bind(at,root)?, "factory WLAN firmware bind is not read-only RAM");
        }
        let _=skyward_sys::kmsg("VENUS_WLAN_FACTORY_MAC_READY source=readonly_persist firmware=verified_ram address_logged=false");
        Ok(())
    }
}
#[cfg(target_os = "android")]
pub use runtime::prepare;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_preparation_is_removed_and_can_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("firmware");
        let failed = publish_staged(&root, |stage| {
            std::fs::write(stage.join("partial"), b"incomplete")?;
            anyhow::bail!("injected copy failure")
        });
        assert!(failed.is_err());
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
        publish_staged(&root, |stage| {
            std::fs::write(stage.join("ready"), b"verified")?;
            Ok(())
        }).unwrap();
        assert_eq!(std::fs::read(root.join("ready")).unwrap(), b"verified");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
    }
    #[test]
    fn failed_publish_keeps_existing_output_and_removes_only_owned_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("firmware");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("existing"), b"retained").unwrap();
        let foreign = tmp.path().join("firmware.new");
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("other"), b"untouched").unwrap();
        assert!(publish_staged(&root, |stage| {
            std::fs::write(stage.join("ready"), b"verified")?;
            Ok(())
        }).is_err());
        assert_eq!(std::fs::read(root.join("existing")).unwrap(), b"retained");
        assert_eq!(std::fs::read(foreign.join("other")).unwrap(), b"untouched");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 2);
    }
    #[test]
    fn preserves_exact_factory_address_in_driver_syntax() {
        assert_eq!(driver_config(b"wlan0=001a2b3c4d5e").unwrap(),b"Intf0MacAddress=001A2B3C4D5E\nEND\n");
        assert_eq!(driver_config(b"wlan0=021a2b3c4d5e\r\n").unwrap(),b"Intf0MacAddress=021A2B3C4D5E\nEND\n");
    }
    #[test]
    fn malformed_or_additional_settings_cannot_become_firmware() {
        for bad in [b"wlan0=001a2b3c4d5e\nwlan1=001a2b3c4d5f".as_slice(),b"wlan0=00:1a:2b:3c:4d:5e",b"wlan0=001a2b3c4d5e ",b"wlan0=001a2b3c4d5",b"wlan0=001a2b3c4d5g",b"wlan1=001a2b3c4d5e",b"wlan0=001a2b3c4d5e\0"] {
            assert!(driver_config(bad).is_err());
        }
    }
    #[test]
    fn rejects_zero_broadcast_and_multicast_addresses() {
        for bad in [b"wlan0=000000000000",b"wlan0=ffffffffffff",b"wlan0=011a2b3c4d5e"] {
            assert!(driver_config(bad).is_err());
        }
    }
}
