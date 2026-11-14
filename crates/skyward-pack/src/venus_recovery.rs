//! Opt-in, self-contained venus Recovery. Normal packaging and diagnostic
//! images keep their previous behavior. The installed candidate restores the
//! original first-stage init; only the temporary test image forces Recovery.
use crate::ramdisk::Ramdisk;
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
const ASSETS: &[&str] = &[
    "binderfs-setup",
    "km41-launch",
    "km41-rpmb",
    "km41-kmsg",
    "runtime.img",
    "vndk30.img",
    "framework-manifest.xml",
    "km41-runtime.sh",
    "ce-runtime.sh",
    "boot-ld.config.txt",
    "boot-runtime.sh",
    "cache-runtime.sh",
    "modem-hold",
    "modem-prepare.sh",
    "network-ld.config.txt",
    "tethering.img",
    "network-suspend",
    "netmgr-ready",
    "network-netd",
];
const SECURITY_RC: &str = "\n# Own the HAL process group independently of the interactive UI.\nservice venus_security /system/bin/sh /skyward/security-start.sh\n    disabled\n    oneshot\n    user root\n    group root system log\n    seclabel u:r:recovery:s0\n";
const MODEM_RC: &str = "\n# The radio owns NV helpers independently; the UI requests its lifecycle.\nservice venus_modem /system/bin/skyward venus-modem\n    disabled\n    oneshot\n    user root\n    group root system radio log\n    seclabel u:r:recovery:s0\n    stdio_to_kmsg\n";
const SECURITY_START: &[u8] = b"#!/system/bin/sh\nset -eu\nexport PATH=/system/bin:/system/xbin\numask 077\nD=/tmp/skyward-recovery\nmkdir -p \"$D\"\nexec >>\"$D/security.log\" 2>&1\nsh /skyward/rw-runtime.sh\necho VENUS_SECURITY_SUPERVISOR_READY\n# init reaps a service's entire process group on exit, even for oneshot.\n# Keep this owner alive; no automatic restart of HALs using live Data.\nwhile :; do sleep 3600; done\n";
const START: &[u8] = b"#!/system/bin/sh\nexport PATH=/system/bin:/system/xbin\numask 077\nD=/tmp/skyward-recovery\nmkdir -p \"$D\" || exit 1\nexec >\"$D/ui.log\" 2>&1\nprintf '%s\\n' \"$$\" >\"$D/pid\"\nn=0\nwhile [ \"$n\" -lt 45 ]; do\n    [ -f /tmp/skyward-probe/done ] && [ -c /dev/dri/card0 ] && break\n    n=$((n+1)); sleep 1\ndone\n[ -f /tmp/skyward-probe/done ] && [ -c /dev/dri/card0 ] || exit 1\necho 'VENUS_RECOVERY_UI_START data_mode=rw credential_input=on_device'\nexec /system/bin/skyward venus-recovery --config /skyward/device.toml\n";

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
            "binderfs-setup"
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
        rd.add(&format!("skyward/runtime/{name}"), mode, bytes)?;
    }
    let script = fs::read(assets.join("rw-runtime.sh"))?;
    ensure!(
        script.starts_with(b"#!/system/bin/sh\n"),
        "invalid integrated runtime script"
    );
    rd.add("skyward/runtime/SHA256SUMS", 0o600, sums.into_bytes())?;
    rd.add("skyward/rw-runtime.sh", 0o700, script)?;
    rd.add("skyward/security-start.sh", 0o700, SECURITY_START.to_vec())?;
    let init_path = "system/etc/init/hw/init.rc";
    let init = std::str::from_utf8(&rd.find(init_path).context("missing recovery init.rc")?.data)?;
    ensure!(
        !init.contains("service venus_security ") && !init.contains("service venus_modem "),
        "independent Venus service already present"
    );
    rd.replace(init_path, format!("{init}{SECURITY_RC}{MODEM_RC}").into_bytes())?;
    rd.add(
        "skyward/venus-recovery.txt",
        0o644,
        format!(
            "venus-recovery-rw-v1\nentry={}\ndata_mode=rw\nowner_ce=gatekeeper_1.0_keymaster_4.1\n",
            if normal_entry {
                "original_init"
            } else {
                "temporary_forced_recovery"
            }
        )
        .into_bytes(),
    )?;
    rd.replace("skyward/ui-start.sh", START.to_vec())?;
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
    fn integrated_start_uses_real_ui_not_probe() {
        let s = std::str::from_utf8(START).unwrap();
        assert!(s.contains("skyward venus-recovery"));
        assert!(!s.contains("ui-probe"));
        for x in ["mkfs", "wipe", "flash", "exec-out", "--credential"] {
            assert!(!s.contains(x));
        }
    }
    #[test]
    fn assets_are_bounded_and_explicit() {
        assert_eq!(ASSETS.len(), 19);
        for x in ASSETS {
            assert!(!x.contains('/') && !x.contains(".."));
        }
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
    fn hal_owner_outlives_setup_and_does_not_restart_on_ui_exit() {
        let script = std::str::from_utf8(SECURITY_START).unwrap();
        assert!(SECURITY_RC.contains("service venus_security "));
        assert!(SECURITY_RC.contains("    disabled\n    oneshot\n"));
        assert!(!SECURITY_RC.contains("restart_period"));
        let setup = script.find("sh /skyward/rw-runtime.sh\n").unwrap();
        let hold = script.find("while :; do sleep 3600; done").unwrap();
        assert!(setup < hold);
        assert!(script.contains("set -eu\n"));
    }
}
