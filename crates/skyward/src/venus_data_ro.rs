//! Non-writing data bring-up after HIDL Keymaster has unwrapped the metadata key.
//! This is deliberately separate from data::open_without_credential(), which
//! may fsck and mount writable. Here every mapping and mount is read-only;
//! the original key directories are never edited or upgraded on disk.
#[cfg(target_os = "android")]
use anyhow::Context;
use anyhow::{Result, ensure};
use skyward_block::fstab::Entry;

#[cfg(target_os = "android")]
const MOUNT: &str = "/mnt/skyward-data-ro";
#[cfg(target_os = "android")]
const MAPPING: &str = "skyward-km41-data-ro";

/// Fail closed for the repeatedly resetting teardown path. Retention is a
/// diagnostic isolation mode, not evidence that unmount has been repaired.
pub fn validate_probe_mode(keep: bool, allow_teardown: bool, _de: bool) -> Result<()> {
    ensure!(
        !(keep && allow_teardown),
        "choose either retention or explicit teardown, not both"
    );
    ensure!(
        keep || allow_teardown,
        "automatic data teardown is gated after repeated resets; use --keep-data-ro for mount-only isolation; --allow-teardown-test is an explicit risky diagnostic, not a fix"
    );
    Ok(())
}

fn validate_entry(entry: &Entry) -> Result<()> {
    ensure!(
        entry.mount_point == "/data" && entry.filesystem == "f2fs",
        "venus read-only probe requires the actual F2FS /data entry"
    );
    ensure!(
        entry.extra("keydirectory") == Some("/metadata/vold/metadata_encryption"),
        "unexpected metadata key directory"
    );
    ensure!(
        entry.extra("metadata_encryption") == Some("aes-256-xts:wrappedkey_v0"),
        "unsupported metadata encryption mode"
    );
    ensure!(
        entry
            .extra("fileencryption")
            .is_some_and(|s| s.contains(":v2+") && s.ends_with("+wrappedkey_v0")),
        "unsupported fscrypt policy"
    );
    Ok(())
}

fn readonly_options() -> Vec<String> {
    [
        "ro",
        "norecovery",
        "inlinecrypt",
        "nosuid",
        "nodev",
        "noatime",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn sectors(size: u64) -> Result<u64> {
    ensure!(
        size >= 4096 && size.is_multiple_of(4096),
        "userdata size is not a whole 4 KiB crypto unit"
    );
    Ok(size / 512)
}

#[cfg(target_os = "android")]
pub fn verify(
    service: &skyward_crypto::keymint::KeyMint<'_>,
    ephemeral: &[u8],
    open_de: bool,
    keep_mounted: bool,
) -> Result<()> {
    use skyward_block::{dm, mount};
    use std::fs::{self, File, OpenOptions};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{FileExt, PermissionsExt};
    use std::path::Path;
    use zeroize::Zeroize;
    ensure!(
        ephemeral.len() == 68,
        "unexpected venus ephemeral wrapped-key size"
    );
    ensure!(
        skyward_vab::snapshot_state()? == skyward_vab::SnapshotState::None,
        "read-only diagnostic requires known idle OTA state"
    );
    ensure!(
        !mount::is_mount_point(Path::new("/data"))?,
        "refusing to interfere with existing /data mount"
    );
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open("/skyward/run/locks/data.lock")?;
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another data probe is running"
    );
    let suffix = skyward_sys::prop("ro.boot.fstab_suffix").unwrap_or_else(|| "default".into());
    ensure!(
        matches!(suffix.as_str(), "default" | "emmc"),
        "unsupported vendor fstab suffix"
    );
    let table = fs::read_to_string(format!("/mnt/skyward-ro/vendor/etc/fstab.{suffix}"))?;
    let entries: Vec<_> = skyward_block::fstab::entries(&table)
        .into_iter()
        .filter(|e| e.mount_point == "/data")
        .collect();
    ensure!(entries.len() == 1, "ambiguous vendor data entry");
    let entry = &entries[0];
    validate_entry(entry)?;
    let device = skyward_block::fstab::device(entry)?;
    ensure!(
        fs::canonicalize(&device)? == fs::canonicalize("/dev/block/by-name/userdata")?,
        "fstab is not this device's userdata"
    );
    ensure!(
        mount::points_of(skyward_block::devnum(&device)?)?.is_empty(),
        "userdata already mounted elsewhere"
    );
    // TWRP keeps the metadata-decrypted device mounted while installing DE
    // and later processing CE. Reuse only our verified receipt in this boot;
    // never remove another process's mount or substitute an unowned mapping.
    if crate::venus_data_session::reuse(&device)? {
        ensure!(
            keep_mounted,
            "existing retained data session cannot be used by a teardown test"
        );
        let (_, minor) = dm::status(MAPPING)?.context("retained mapping disappeared")?;
        let mut magic = [0u8; 4];
        File::open(format!("/dev/block/dm-{minor}"))?.read_exact_at(&mut magic, 1024)?;
        ensure!(
            u32::from_le_bytes(magic) == 0xf2f52010,
            "retained session no longer has an F2FS superblock"
        );
        println!("KEYMASTER_DECRYPTED_F2FS_SUPERBLOCK_OK");
        println!("KEYMASTER_DATA_RO_MOUNT_OK; original /data remains untouched");
        println!("KEYMASTER_DATA_SESSION_REUSED; boot_device_mount_identity=verified");
        println!("KEYMASTER_DATA_SESSION_READY; readonly=true; retained=true");
        if open_de {
            verify_de(service, Path::new(MOUNT))?;
        }
        report_retained(open_de);
        return Ok(());
    }
    ensure!(
        !(keep_mounted && open_de),
        "DE requires an already-verified retained session; run --data-ro-keep first"
    );
    let version =
        dm::target_version("default-key")?.context("kernel has no dm-default-key target")?;
    println!("KEYMASTER_DM_DEFAULT_KEY_PRESENT version={version:?}");
    let mut target = dm::Target::DefaultKey {
        sectors: sectors(skyward_block::size_bytes(&device)?)?,
        device: device.clone(),
        cipher: "aes-xts-plain64".into(),
        key: ephemeral.iter().map(|b| format!("{b:02x}")).collect(),
        wrapped: true,
    };
    let created = dm::create(MAPPING, std::slice::from_ref(&target), true);
    if let dm::Target::DefaultKey { key, .. } = &mut target {
        key.zeroize();
    }
    let mapped = created.context("create read-only encrypted mapping")?;
    let mut guard = MappingGuard {
        mounted: false,
        active: true,
        attempted: false,
        retained: false,
    };
    crate::venus_keymaster::phase("DATA_RO_MAPPING_CREATED");
    let work = (|| -> Result<()> {
        ensure!(dm::readonly(MAPPING)?, "mapping is not read-only");
        let mut magic = [0u8; 4];
        File::open(&mapped)?.read_exact_at(&mut magic, 1024)?;
        ensure!(
            u32::from_le_bytes(magic) == 0xf2f52010,
            "decrypted data does not have the expected F2FS superblock"
        );
        println!("KEYMASTER_DECRYPTED_F2FS_SUPERBLOCK_OK");
        if let Ok(meta) = fs::symlink_metadata(MOUNT) {
            ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "unsafe diagnostic mount point"
            );
        }
        fs::create_dir_all(MOUNT)?;
        fs::set_permissions(MOUNT, fs::Permissions::from_mode(0o700))?;
        mount::mount(&mapped, Path::new(MOUNT), "f2fs", &readonly_options())
            .context("mount data read-only without recovery")?;
        guard.mounted = true;
        // Even an error while inspecting the new mount must not re-enter
        // the previously resetting teardown path via Drop.
        if keep_mounted {
            guard.retain()?;
        }
        crate::venus_keymaster::phase("DATA_RO_MOUNTED");
        let flags = fs::read_to_string("/proc/mounts")?;
        ensure!(
            flags.lines().any(|line| {
                let p: Vec<_> = line.split_whitespace().collect();
                p.len() > 3 && p[1] == MOUNT && p[3].split(',').any(|o| o == "ro")
            }),
            "read-only mount verification failed"
        );
        println!("KEYMASTER_DATA_RO_MOUNT_OK; original /data remains untouched");
        if keep_mounted {
            crate::venus_data_session::save(&device)?;
            println!("KEYMASTER_DATA_SESSION_READY; readonly=true; retained=true");
        }
        if open_de {
            verify_de(service, Path::new(MOUNT))?;
        }
        Ok(())
    })();
    if keep_mounted && guard.mounted {
        // This path intentionally leaves an owned, read-only mount and DM
        // mapping alive. On validation errors, keep them inspectable as well;
        // never disguise an error as a successful mount or enter teardown.
        guard.retain()?;
        if work.is_ok() {
            report_retained(open_de);
        } else {
            crate::venus_keymaster::phase("DATA_RO_RETAINED_WITH_ERROR");
            eprintln!(
                "read-only diagnostic resources retained after a verification error; no unmount was attempted"
            );
        }
        return work;
    }
    crate::venus_keymaster::phase("DATA_RO_CLEANUP_BEGIN");
    let cleanup = guard.close();
    if cleanup.is_ok() {
        println!("KEYMASTER_DATA_RO_CLEANUP_OK");
    }
    match (work, cleanup) {
        (Ok(()), result) => result,
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup_error)) => {
            Err(error.context(format!("cleanup also failed: {cleanup_error:#}")))
        }
    }
}

#[cfg(target_os = "android")]
fn report_retained(open_de: bool) {
    crate::venus_keymaster::phase("DATA_RO_RETAINED");
    let de = if open_de {
        "verified_owner0"
    } else {
        "not_attempted"
    };
    println!(
        "KEYMASTER_DATA_RO_RETAINED; path={MOUNT}; cleanup=not_attempted; DE={de}; CE=not_attempted"
    );
}

/// Tracks diagnostic resources. A failed cleanup is not silently retried
/// by Drop: another kernel teardown call would obscure the original failure.
struct MappingGuard {
    mounted: bool,
    active: bool,
    attempted: bool,
    retained: bool,
}
impl MappingGuard {
    fn retain(&mut self) -> Result<()> {
        ensure!(
            self.mounted && self.active && !self.attempted,
            "cannot retain absent or partially cleaned resources"
        );
        self.retained = true;
        Ok(())
    }

    fn should_cleanup_on_drop(&self) -> bool {
        !self.attempted && !self.retained
    }

    fn close_with(
        &mut self,
        phase: impl Fn(&str),
        unmount: impl FnOnce() -> Result<()>,
        remove: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        ensure!(
            !self.retained,
            "read-only diagnostic mount retained intentionally; automatic cleanup is not allowed"
        );
        if self.attempted {
            ensure!(
                !self.mounted && !self.active,
                "previous cleanup did not finish; refusing an automatic retry"
            );
            return Ok(());
        }
        self.attempted = true;
        if self.mounted {
            phase("DATA_RO_UMOUNT_BEGIN");
            if let Err(error) = unmount() {
                phase("DATA_RO_UMOUNT_FAILED");
                return Err(error);
            }
            self.mounted = false;
            phase("DATA_RO_UMOUNT_OK");
        }
        if self.active {
            phase("DATA_RO_DM_REMOVE_BEGIN");
            if let Err(error) = remove() {
                phase("DATA_RO_DM_REMOVE_FAILED");
                return Err(error);
            }
            self.active = false;
            phase("DATA_RO_DM_REMOVE_OK");
        }
        Ok(())
    }
}
#[cfg(target_os = "android")]
impl MappingGuard {
    fn close(&mut self) -> Result<()> {
        self.close_with(
            crate::venus_keymaster::phase,
            || skyward_block::mount::umount(std::path::Path::new(MOUNT)),
            || skyward_block::dm::remove(MAPPING),
        )
    }
}
#[cfg(target_os = "android")]
impl Drop for MappingGuard {
    fn drop(&mut self) {
        if self.should_cleanup_on_drop() {
            if let Err(e) = self.close() {
                eprintln!("KEYMASTER_DATA_RO_CLEANUP_FAILED: {e:#}");
            }
        }
    }
}

#[cfg(target_os = "android")]
pub(crate) fn verify_de(
    service: &skyward_crypto::keymint::KeyMint<'_>,
    mount: &std::path::Path,
) -> Result<()> {
    use skyward_crypto::{
        fscrypt,
        keystore::{StoredKey, Unwrap},
    };
    use std::fs;
    use zeroize::Zeroizing;
    let mode = fs::read_to_string(mount.join("unencrypted/mode"))?;
    ensure!(
        fscrypt::hardware_wrapped(mode.trim()),
        "data file keys are not in the verified wrapped format"
    );
    let install = |dir: &std::path::Path| -> Result<[u8; fscrypt::NAME_BYTES]> {
        let raw = Zeroizing::new(StoredKey::load(dir)?.unwrap(Unwrap::Hardware(service))?);
        let ephemeral = Zeroizing::new(service.ephemeral_storage_key(&raw)?);
        fscrypt::add_key(mount, &ephemeral, true)
    };
    crate::venus_keymaster::phase("DEVICE_KEY_INSTALL_BEGIN");
    let name = install(&mount.join("unencrypted/key"))
        .context("install device-wide fscrypt key")?;
    ensure!(
        name == fscrypt::recorded_name(&mount.join("unencrypted/ref"))?,
        "device-wide fscrypt identifier mismatch"
    );
    println!("KEYMASTER_DEVICE_ENCRYPTED_KEY_OK identifier_matches=true");
    let mut users = Vec::new();
    for e in fs::read_dir(mount.join("misc/vold/user_keys/de"))? {
        let e = e?;
        if let Some(id) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) {
            users.push(id);
        }
    }
    users.sort_unstable();
    ensure!(users.len() <= 128, "unexpected user count");
    // Bring-up is restricted to the owner. Do not walk work-profile/clone
    // users implicitly; they have separate lifetimes and credential rules.
    println!(
        "KEYMASTER_DE_USERS_PRESENT count={} probe_user=0",
        users.len()
    );
    ensure!(users.contains(&0), "owner DE key directory missing");
    for user in [0u32] {
        crate::venus_keymaster::phase("OWNER_DE_INSTALL_BEGIN");
        let dir = mount.join(format!("misc/vold/user_keys/de/{user}"));
        install(&dir).with_context(|| format!("install user {user} DE key"))?;
        crate::venus_keymaster::phase("OWNER_DE_INSTALL_OK");
        let system = mount.join(format!("system_de/{user}"));
        ensure!(
            fscrypt::unlocked(&system)?,
            "user DE key not accepted by filesystem"
        );
        // Do not list personal filenames or read credential contents. Just
        // confirm that the appropriate protected directory is now accessible.
        let count = fs::read_dir(&system)?.count();
        let media = mount.join(format!("media/{user}"));
        let ce = if media.exists() {
            fscrypt::unlocked(&media)?
        } else {
            false
        };
        println!(
            "KEYMASTER_USER_DE_OK user={user} system_de_entries={count} shared_storage_unlocked={ce}"
        );
    }
    crate::venus_keymaster::phase("OWNER_DE_CHECK_COMPLETE");
    println!("KEYMASTER_DE_CHAIN_OK; no CE credential was submitted");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_orders_unmount_before_mapping_removal() {
        use std::cell::RefCell;
        let steps = RefCell::new(Vec::new());
        let mut g = MappingGuard {
            mounted: true,
            active: true,
            attempted: false,
            retained: false,
        };
        g.close_with(
            |s| steps.borrow_mut().push(s.to_owned()),
            || {
                steps.borrow_mut().push("unmount".into());
                Ok(())
            },
            || {
                steps.borrow_mut().push("remove".into());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            *steps.borrow(),
            [
                "DATA_RO_UMOUNT_BEGIN",
                "unmount",
                "DATA_RO_UMOUNT_OK",
                "DATA_RO_DM_REMOVE_BEGIN",
                "remove",
                "DATA_RO_DM_REMOVE_OK"
            ]
        );
        assert!(!g.mounted && !g.active && g.attempted);
        g.close_with(
            |_| panic!("duplicate phase"),
            || panic!("duplicate unmount"),
            || panic!("duplicate remove"),
        )
        .unwrap();
    }
    #[test]
    fn unmount_failure_never_removes_live_mapping_or_retries() {
        let mut g = MappingGuard {
            mounted: true,
            active: true,
            attempted: false,
            retained: false,
        };
        assert!(
            g.close_with(
                |_| {},
                || anyhow::bail!("busy"),
                || panic!("mapping removed beneath mounted filesystem")
            )
            .is_err()
        );
        assert!(g.mounted && g.active && g.attempted);
        assert!(
            g.close_with(
                |_| panic!("automatic retry"),
                || panic!("automatic retry"),
                || panic!("automatic retry")
            )
            .is_err()
        );
    }
    #[test]
    fn remove_failure_keeps_truthful_state_and_does_not_repeat_unmount() {
        let mut g = MappingGuard {
            mounted: true,
            active: true,
            attempted: false,
            retained: false,
        };
        assert!(
            g.close_with(|_| {}, || Ok(()), || anyhow::bail!("busy"))
                .is_err()
        );
        assert!(!g.mounted && g.active && g.attempted);
        assert!(
            g.close_with(
                |_| panic!("automatic retry"),
                || panic!("automatic retry"),
                || panic!("automatic retry")
            )
            .is_err()
        );
    }
    #[test]
    fn failure_before_mount_removes_only_the_owned_mapping() {
        let mut g = MappingGuard {
            mounted: false,
            active: true,
            attempted: false,
            retained: false,
        };
        g.close_with(|_| {}, || panic!("unmount without mount"), || Ok(()))
            .unwrap();
        assert!(!g.mounted && !g.active);
    }
    #[test]
    fn no_resources_means_no_syscalls() {
        let mut g = MappingGuard {
            mounted: false,
            active: false,
            attempted: false,
            retained: false,
        };
        g.close_with(
            |_| panic!("unexpected phase"),
            || panic!("unmount"),
            || panic!("remove"),
        )
        .unwrap();
    }

    #[test]
    fn retained_mount_never_enters_unmount_or_remove() {
        let mut g = MappingGuard {
            mounted: true,
            active: true,
            attempted: false,
            retained: false,
        };
        g.retain().unwrap();
        assert!(g.mounted && g.active && g.retained);
        assert!(!g.should_cleanup_on_drop());
        assert!(
            g.close_with(
                |_| panic!("phase"),
                || panic!("unmount"),
                || panic!("remove")
            )
            .is_err()
        );
    }
    #[test]
    fn cannot_retain_resources_that_were_never_mounted_or_already_cleaned() {
        for (mounted, active, attempted) in [
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ] {
            let mut g = MappingGuard {
                mounted,
                active,
                attempted,
                retained: false,
            };
            assert!(g.retain().is_err());
            assert!(!g.retained);
        }
    }
    #[test]
    fn teardown_requires_consent_and_retained_de_requires_a_session_at_runtime() {
        assert!(validate_probe_mode(false, false, false).is_err());
        assert!(validate_probe_mode(false, false, true).is_err());
        assert!(validate_probe_mode(true, false, false).is_ok());
        assert!(validate_probe_mode(true, false, true).is_ok());
        assert!(validate_probe_mode(false, true, false).is_ok());
        assert!(validate_probe_mode(false, true, true).is_ok());
        assert!(validate_probe_mode(true, true, false).is_err());
    }

    fn entry() -> Entry {
        skyward_block::fstab::entries("/dev/block/by-name/userdata /data f2fs rw,discard wait,check,formattable,keydirectory=/metadata/vold/metadata_encryption,metadata_encryption=aes-256-xts:wrappedkey_v0,fileencryption=aes-256-xts:aes-256-cts:v2+inlinecrypt_optimized+wrappedkey_v0").remove(0)
    }
    #[test]
    fn verified_vendor_layout_is_accepted() {
        assert!(validate_entry(&entry()).is_ok());
    }
    #[test]
    fn unknown_metadata_cipher_is_refused() {
        let mut e = entry();
        e.extras.retain(|x| !x.starts_with("metadata_encryption="));
        assert!(validate_entry(&e).is_err());
    }
    #[test]
    fn mount_is_never_writable_or_replayed() {
        let o = readonly_options();
        assert!(o.contains(&"ro".into()) && o.contains(&"norecovery".into()));
        for bad in ["rw", "discard", "check", "formattable"] {
            assert!(!o.contains(&bad.into()));
        }
    }
    #[test]
    fn sectors_keep_four_kib_units() {
        assert_eq!(sectors(8192).unwrap(), 16);
        assert!(sectors(4097).is_err());
        assert!(sectors(0).is_err());
    }
}
