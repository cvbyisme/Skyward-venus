//! Opt-in, self-contained venus Recovery. Normal packaging and diagnostic
//! images keep their previous behavior. The installed candidate restores the
//! original first-stage init; only the temporary test image forces Recovery.
use crate::ramdisk::Ramdisk;
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
const ASSETS: &[&str] = &[
    "venus-runtime",
    "binderfs-setup",
    "crypto-launch",
    "rw-runtime",
    "crypto-start",
    "km41-launch",
    "km41-rpmb",
    "km41-kmsg",
    "runtime.img",
    "vndk30.img",
    "framework-manifest.xml",
    "km41-runtime",
    "ce-runtime",
    "boot-ld.config.txt",
    "boot-runtime",
    "cache-runtime",
    "modem-hold",
    "modem-prepare",
    "network-ld.config.txt",
    "tethering.img",
    "network-suspend",
    "netmgr-ready",
    "network-netd",
];
const SECURITY_RC: &str = "\n# Own the HAL process group independently of the interactive UI.\nservice venus_security /system/bin/skyward venus-helper security-start\n    disabled\n    oneshot\n    user root\n    group root system keystore readproc log\n    seclabel u:r:recovery:s0\n";
// The forced-Recovery image does not enter Android's normal boot lifecycle.
// Start our owned security supervisor as soon as the init boot action runs,
// concurrently with the UI. This removes the UI/navigation dependency from
// the critical path to the first Data decrypt attempt.
const SECURITY_EARLY_RC: &str =
    "\n# Early crypto readiness for temporary Recovery only.\non boot\n    start venus_security\n";
// Android 11's original legacy wait_for_keymaster service expects platform
// libbinder in /system. This independent Recovery supplies its own verified
// HAL+Keystore services, so that obsolete process only crashes/restarts every
// five seconds. Never change its definition in normal-entry images.
const LEGACY_WAIT_RC: &str = "system/etc/init/wait_for_keymaster.rc";
const LEGACY_WAIT_DISABLED: &str = "# Venus temporary Recovery: legacy wait_for_keymaster is replaced by\n# venus_security (Keymaster 4.1, Gatekeeper, Keystore2).\n# No service is declared here: no repeated restart on missing libbinder.\n";
const MODEM_RC: &str = "\n# The radio owns NV helpers independently; the UI requests its lifecycle.\nservice venus_modem /system/bin/skyward venus-modem\n    disabled\n    oneshot\n    user root\n    group root system radio log\n    seclabel u:r:recovery:s0\n    stdio_to_kmsg\n";

/// Build the persistent *normal-Android-entry* installer capsule from the
/// temporary running Recovery's ramdisk. Only the temporary carrier disables
/// the legacy Keymaster loop and autostarts the Recovery crypto owner.
/// The installed candidate must keep those Android init semantics unchanged.
pub fn restore_normal_init_for_capsule(rd: &mut Ramdisk, stock_wait: &[u8]) -> Result<()> {
    let init_path = "system/etc/init/hw/init.rc";
    let current = std::str::from_utf8(
        &rd.find(init_path)
            .context("missing temporary Recovery init rc")?
            .data,
    )?;
    let normal = current
        .strip_suffix(SECURITY_EARLY_RC)
        .context("temporary Recovery security trigger missing at end of init rc")?;
    ensure!(
        normal.contains(SECURITY_RC) && normal.contains(MODEM_RC),
        "normal capsule would lose owned Recovery services"
    );
    ensure!(
        std::str::from_utf8(stock_wait)?.lines().next()
            == Some("service wait_for_keymaster /system/bin/wait_for_keymaster"),
        "stock wait_for_keymaster definition changed"
    );
    rd.replace(init_path, normal.as_bytes().to_vec())?;
    rd.replace(LEGACY_WAIT_RC, stock_wait.to_vec())?;
    Ok(())
}

pub fn prepare(
    rd: &mut Ramdisk,
    assets: &Path,
    normal_entry: bool,
    original_init: &[u8],
) -> Result<()> {
    ensure!(
        rd.find("skyward/basic-features.txt").is_some(),
        "integrated Recovery requires the verified venus probe/UI/basic setup"
    );
    ensure!(
        rd.find("skyward/venus-recovery.txt").is_none(),
        "integrated Recovery is already present"
    );
    // Preserve the original policy's decisions and add only the Keystore 2.0
    // vocabulary/rules verified by prepare-crypto-policy.py for this carrier.
    let original_policy = &rd.find("sepolicy").context("missing recovery policy")?.data;
    let policy_source = fs::read_to_string(assets.join("recovery-sepolicy.source.sha256"))?;
    ensure!(
        policy_source.trim() == format!("{:x}", Sha256::digest(original_policy)),
        "crypto policy was built for another recovery carrier"
    );
    let policy = fs::read(assets.join("recovery-sepolicy"))?;
    ensure!(
        policy.starts_with(&0xf97cff8cu32.to_le_bytes()) && policy.len() <= 4 * 1024 * 1024,
        "invalid recovery crypto policy"
    );
    rd.replace("sepolicy", policy)?;
    // The reference contributes security service/tool dependencies only.
    // Skyward remains the recovery and owns the Data decryption lifecycle.
    let crypto = crate::ramdisk::Ramdisk::decode_bounded(
        &fs::read(assets.join("crypto-runtime.cpio.lz4"))?,
        128 * 1024 * 1024,
        4096,
    )?;
    for entry in crypto.entries.iter().filter(|e| !e.is_dir()) {
        ensure!(
            entry.mode & 0o170000 == 0o100000
                && Path::new(&entry.name)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
                && (entry.name.starts_with("system/lib64/")
                    || entry.name.starts_with("system/bin/")
                    || entry.name.starts_with("system/etc/")
                    || entry.name == "file_contexts"),
            "invalid crypto asset {} mode={:#o}",
            entry.name,
            entry.mode
        );
        rd.add(
            &format!("skyward/crypto-runtime/{}", entry.name),
            entry.mode & 0o777,
            entry.data.clone(),
        )?;
    }
    rd.add(
        "skyward/crypto-runtime/SOURCE.sha256",
        0o600,
        fs::read(assets.join("crypto-runtime.SOURCE.sha256"))?,
    )?;
    let mut sums = String::new();
    for &name in ASSETS {
        let path = assets.join(name);
        let m =
            fs::symlink_metadata(&path).with_context(|| format!("missing runtime asset {name}"))?;
        ensure!(
            m.is_file()
                && !m.file_type().is_symlink()
                && m.len() > 0
                && m.len() <= 192 * 1024 * 1024,
            "unsafe runtime asset {name}"
        );
        let bytes = fs::read(&path)?;
        if matches!(
            name,
            "venus-runtime"
                | "modem-prepare"
                | "binderfs-setup"
                | "crypto-launch"
                | "rw-runtime"
                | "crypto-start"
                | "km41-runtime"
                | "ce-runtime"
                | "boot-runtime"
                | "cache-runtime"
                | "km41-launch"
                | "km41-rpmb"
                | "km41-kmsg"
                | "modem-hold"
                | "network-suspend"
                | "netmgr-ready"
                | "network-netd"
        ) {
            ensure!(
                bytes.starts_with(b"\x7fELF\x02\x01") && bytes.get(18..20) == Some(&[0xb7, 0x00]),
                "runtime helper is not ARM64: {name}"
            );
        }
        sums.push_str(&format!("{:x}  {name}\n", Sha256::digest(&bytes)));
        let mode = if matches!(name, "boot-ld.config.txt" | "network-ld.config.txt") {
            // Vendor Wi-Fi helpers run as system/wifi. This contains
            // library namespace rules, and must be readable after their
            // uid drop, just like Android's installed linker config.
            0o644
        } else if name.ends_with(".img") || name.ends_with(".xml") || name.ends_with(".txt") {
            0o600
        } else {
            0o700
        };
        // Static Rust helpers are one multicall executable. Keep one inode
        // in the image so the migration stays within the boot partition.
        if name != "venus-runtime"
            && rd
                .find("skyward/runtime/venus-runtime")
                .is_some_and(|entry| entry.data == bytes)
        {
            rd.link(&format!("skyward/runtime/{name}"), "venus-runtime")?;
        } else {
            rd.add(&format!("skyward/runtime/{name}"), mode, bytes)?;
        }
    }
    rd.add("skyward/runtime/SHA256SUMS", 0o600, sums.into_bytes())?;
    let init_path = "system/etc/init/hw/init.rc";
    let init = std::str::from_utf8(&rd.find(init_path).context("missing recovery init.rc")?.data)?;
    ensure!(
        !init.contains("service venus_security ") && !init.contains("service venus_modem "),
        "independent Venus service already present"
    );
    rd.replace(
        init_path,
        format!(
            "{init}{SECURITY_RC}{MODEM_RC}{}",
            if normal_entry { "" } else { SECURITY_EARLY_RC }
        )
        .into_bytes(),
    )?;
    if !normal_entry {
        // The old service has no `disabled` or `oneshot` and init respawns it
        // forever. Replacing only in the temporary Recovery leaves the normal
        // system boot contract unchanged. Refuse unexpected source content.
        //
        // A carrier that declares no such service at all (a third-party
        // recovery image, for one) has nothing that could respawn, so there is
        // nothing to take away and nothing to refuse.
        match rd.find(LEGACY_WAIT_RC) {
            Some(entry) => {
                let legacy = std::str::from_utf8(&entry.data)?;
                ensure!(
                    legacy.lines().next()
                        == Some("service wait_for_keymaster /system/bin/wait_for_keymaster")
                        && !legacy.contains("override")
                        && !legacy.contains("on boot"),
                    "unexpected legacy Keymaster service definition"
                );
                rd.replace(LEGACY_WAIT_RC, LEGACY_WAIT_DISABLED.as_bytes().to_vec())?;
            }
            None => eprintln!(
                "VENUS_RECOVERY_NO_LEGACY_WAIT the carrier declares no legacy Keymaster service"
            ),
        }
    }
    rd.add(
        "skyward/venus-recovery.txt",
        0o644,
        format!(
            "venus-recovery-rw-v1\nentry={}\ndata_mode=rw\nowner_ce=keystore2_authorization_keymint_compat\n",
            if normal_entry {
                "original_init"
            } else {
                "temporary_forced_recovery"
            }
        )
        .into_bytes(),
    )?;
    rd.replace("skyward/ui-autostart.txt",b"venus-ui-autostart-v1\nIntegrated venus Recovery; hardware authentication and writable Data.\n".to_vec())?;
    rd.add(
        skyward_config::ramdisk::MTP_RC,
        0o644,
        include_bytes!("mtp.rc").to_vec(),
    )?;
    if normal_entry {
        rd.replace("system/bin/init", original_init.to_vec())?;
    }
    eprintln!(
        "VENUS_RECOVERY_BUILT data_mode=rw bundled_hal_runtime=true original_first_stage={normal_entry}; hardware cold-boot validation remains required"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn assets_are_bounded_and_explicit() {
        assert_eq!(ASSETS.len(), 23);
        for x in ASSETS {
            assert!(!x.contains('/') && !x.contains(".."));
        }
    }
    #[test]
    fn temporary_recovery_prestarts_data_services_without_legacy_loop() {
        assert!(SECURITY_EARLY_RC.contains("on boot\n    start venus_security"));
        assert!(!SECURITY_RC.contains("on boot"));
        assert!(LEGACY_WAIT_DISABLED.contains("venus_security"));
        assert!(
            !LEGACY_WAIT_DISABLED
                .lines()
                .any(|line| line.starts_with("service "))
        );
        assert_eq!(LEGACY_WAIT_RC, "system/etc/init/wait_for_keymaster.rc");
        // `prepare` applies both changes only when normal_entry=false.
        let source = include_str!("venus_recovery.rs");
        assert!(source.contains("if !normal_entry {"));
    }
    #[test]
    fn modem_owner_is_explicit_independent_and_does_not_restart() {
        assert!(MODEM_RC.contains("service venus_modem /system/bin/skyward venus-modem\n"));
        assert!(MODEM_RC.contains("    disabled\n    oneshot\n"));
        assert!(!MODEM_RC.contains("on boot") && !MODEM_RC.contains("restart_period"));
        assert!(!MODEM_RC.contains("venus_ui") && !MODEM_RC.contains("/system/bin/sh"));
        assert!(MODEM_RC.contains("    group root system radio log\n"));
    }
    #[test]
    fn hal_owner_is_rust_and_independent() {
        assert!(
            SECURITY_RC.contains(
                "service venus_security /system/bin/skyward venus-helper security-start\n"
            )
        );
        assert!(SECURITY_RC.contains("    disabled\n    oneshot\n"));
        assert!(!SECURITY_RC.contains("restart_period"));
    }
}
