//! Optional isolated menus for the already guarded temporary venus probe.
//! Ordinary packaging and the CLI-only probe must not acquire this service.
use crate::ramdisk::Ramdisk;
use anyhow::{Context, Result, ensure};

const INIT: &str = "system/etc/init/hw/init.rc";
const RC: &str = include_str!("venus_ui.rc");
const START: &[u8] = include_bytes!("venus_ui_start.sh");
const MARKER: &[u8] = b"venus-ui-autostart-v1\nTEMPORARY UI TEST ONLY - DO NOT FLASH\nSystem effects remain disabled.\n";

fn append_rc(input: &str) -> Result<String> {
    ensure!(
        input
            .matches("service recovery /system/bin/sh /skyward/probe.sh\n")
            .count()
            == 1,
        "venus UI requires the guarded diagnostic recovery service"
    );
    ensure!(
        !input.contains("service venus_ui "),
        "venus UI service already exists"
    );
    let mut result = input.to_owned();
    result.push_str(RC);
    Ok(result)
}

pub fn prepare(rd: &mut Ramdisk) -> Result<()> {
    ensure!(
        rd.find("skyward/probe-entryfix.txt").is_some(),
        "venus UI requires entryfix probe"
    );
    let input = std::str::from_utf8(&rd.find(INIT).context("missing recovery init.rc")?.data)?;
    let rc = append_rc(input)?;
    rd.add("skyward/ui-autostart.txt", 0o644, MARKER.to_vec())?;
    rd.add("skyward/ui-start.sh", 0o755, START.to_vec())?;
    rd.replace(INIT, rc.into_bytes())?;
    eprintln!(
        "TEMPORARY venus UI: isolated menus start after diagnostics; do not flash."
    );
    Ok(())
}

fn basic_script() -> Result<Vec<u8>> {
    let script = std::str::from_utf8(START)?;
    let old = "--until-stopped\n";
    ensure!(
        script.matches(old).count() == 1,
        "unexpected UI startup command"
    );
    Ok(script
        .replacen(old, "--until-stopped --basic-features\n", 1)
        .into_bytes())
}

pub fn enable_basic(rd: &mut Ramdisk) -> Result<()> {
    ensure!(
        rd.find("skyward/ui-autostart.txt").is_some(),
        "basic functions require autostart UI"
    );
    rd.replace("skyward/ui-start.sh", basic_script()?)?;
    rd.replace("skyward/ui-autostart.txt", b"venus-ui-autostart-v1\nTEMPORARY BASIC FUNCTIONS - DO NOT FLASH\nRead-only mounts and settings enabled; no flash/wipe/decryption.\n".to_vec())?;
    rd.add(
        "skyward/basic-features.txt",
        0o644,
        b"venus-basic-v1\n".to_vec(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basics_only_add_the_explicit_runtime_flag() {
        let original = std::str::from_utf8(START).unwrap();
        let basic = String::from_utf8(basic_script().unwrap()).unwrap();
        assert_eq!(basic.replace(" --basic-features", ""), original);
        assert!(!original.contains("--basic-features"));
    }

    #[test]
    fn ui_is_a_separate_oneshot_service_and_preserves_probe() {
        let original =
            "service recovery /system/bin/sh /skyward/probe.sh\n    disabled\n    oneshot\n";
        let out = append_rc(original).unwrap();
        assert!(out.starts_with(original));
        assert_eq!(out.matches("service venus_ui ").count(), 1);
        assert!(out.contains(
            "service venus_ui /system/bin/sh /skyward/ui-start.sh\n    disabled\n    oneshot\n"
        ));
        assert!(out.contains("on boot\n    start venus_ui\n"));
        assert!(!RC.contains("critical") && !RC.contains("reboot_on_failure"));
        assert!(!RC.contains("stop adbd") && !RC.contains("sys.usb"));
    }

    #[test]
    fn unknown_and_already_modified_rc_are_refused() {
        assert!(append_rc("service recovery /system/bin/recovery\n").is_err());
        let once = append_rc("service recovery /system/bin/sh /skyward/probe.sh\n").unwrap();
        assert!(append_rc(&once).is_err());
    }

    #[test]
    fn wrapper_waits_for_diagnostics_and_executes_only_isolated_ui() {
        let script = std::str::from_utf8(START).unwrap();
        assert!(script.contains("/tmp/skyward-probe/done"));
        assert!(script.contains("[ \"$n\" -lt 45 ]"));
        assert!(script.contains(
            "exec /system/bin/skyward ui-probe --config /skyward/device.toml --until-stopped"
        ));
        assert!(!script.contains("/system/bin/recovery "));
        for forbidden in [
            "setprop sys.usb",
            "stop adbd",
            "mount ",
            "reboot",
            " mkfs",
            "dd if=",
        ] {
            assert!(
                !script.contains(forbidden),
                "unexpected operation: {forbidden}"
            );
        }
    }
}
