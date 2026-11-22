use crate::ramdisk::Ramdisk;
use anyhow::{ensure, Context, Result};

const INIT: &str = "system/etc/init/hw/init.rc";
const STOCK_SERVICE: &str = "service recovery /system/bin/recovery\n";
const PROBE_SERVICE: &str =
    "service recovery /system/bin/skyward venus-helper probe\n    disabled\n    oneshot\n";
/// A third-party carrier declares its recovery service in its own file, and
/// that service is what launches it, so only the command is swapped and the
/// service keeps its options ("socket recovery", the seclabel) and its
/// `oneshot` behaviour.
const CARRIER_SERVICE: &str =
    "service recovery /system/bin/skyward venus-helper probe\n    oneshot\n";

fn patch_init(input: &str) -> Result<String> {
    ensure!(
        input.matches(STOCK_SERVICE).count() == 1,
        "unexpected recovery service"
    );
    let factory = "import /miui.factoryreset.rc\n";
    ensure!(
        input.matches(factory).count() == 1,
        "unexpected factory-reset import"
    );
    let mut text = input.replacen(STOCK_SERVICE, PROBE_SERVICE, 1);
    text = text.replacen(factory, "# Factory-reset hooks disabled for venus probe.\n", 1);
    text.push_str(include_str!("venus_probe.rc"));
    Ok(text)
}

/// Swap the command of whichever file of a third-party carrier starts its
/// recovery service, leaving that carrier's own init and programs alone: they
/// already boot as recovery, so neither the first-stage patch nor the
/// `recovery` link that keeps argv[0] honest is needed.
fn patch_carrier(rd: &mut Ramdisk) -> Result<()> {
    let mut found: Option<(String, String)> = None;
    for entry in &rd.entries {
        if !entry.name.ends_with(".rc") || entry.is_dir() {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&entry.data) else {
            continue;
        };
        if !text.contains(STOCK_SERVICE) {
            continue;
        }
        ensure!(found.is_none(), "the recovery service is declared twice");
        found = Some((
            entry.name.clone(),
            text.replacen(STOCK_SERVICE, CARRIER_SERVICE, 1),
        ));
    }
    let (name, text) = found.context("this carrier starts no recovery service")?;
    rd.replace(&name, text.into_bytes())?;
    eprintln!("VENUS_PROBE_CARRIER service replaced in {name}; carrier init kept as it is");
    let init = rd.find(INIT).context("missing carrier init.rc")?;
    let text = std::str::from_utf8(&init.data)?;
    let text = format!("{text}{}", include_str!("venus_probe.rc"));
    rd.replace(INIT, text.into_bytes())?;
    Ok(())
}

pub fn prepare(rd: &mut Ramdisk, third_party: bool) -> Result<()> {
    if third_party {
        patch_carrier(rd)?;
        rd.add(
            "skyward/probe-entryfix.txt",
            0o644,
            b"venus-entryfix-v1\nTEMPORARY PROBE ONLY - DO NOT FLASH\ncarrier_init=kept\n"
                .to_vec(),
        )?;
        eprintln!("TEMPORARY venus probe: third-party carrier, its own init runs the probe. Do not flash.");
        return Ok(());
    }
    let input = std::str::from_utf8(&rd.find(INIT).context("missing recovery init.rc")?.data)?;
    let text = patch_init(input)?;
    let old_init = &rd.find("system/bin/init").context("missing first-stage init")?.data;
    let new_init = crate::venus_first_stage::patch(old_init)?;
    rd.replace("system/bin/init", new_init)?;
    rd.replace(INIT, text.into_bytes())?;
    let manifest = format!(
        "venus-entryfix-v1\nTEMPORARY PROBE ONLY - DO NOT FLASH\nsource_init_sha256={}\npatched_init_sha256={}\n",
        crate::venus_first_stage::SOURCE_SHA256,
        crate::venus_first_stage::PATCHED_SHA256,
    );
    rd.add("skyward/probe-entryfix.txt", 0o644, manifest.into_bytes())?;
    eprintln!("TEMPORARY venus probe: two verified init branch instructions patched; Kernel unchanged. Do not flash.");
    // argv[0] must not be "recovery", even when asking for --help.
    rd.link("system/bin/skyward", "recovery")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_does_not_start_full_recovery() {
        let s = "import /miui.factoryreset.rc\nservice recovery /system/bin/recovery\n    seclabel u:r:recovery:s0\n";
        let out = patch_init(s).unwrap();
        assert!(!out.contains("import /miui.factoryreset.rc"));
        assert!(out.contains("service recovery /system/bin/skyward venus-helper probe"));
        assert!(out.contains("setprop sys.usb.config adb"));
        assert!(out.contains("    disabled\n    oneshot\n"));
        assert!(out.contains("    seclabel u:r:recovery:s0\n"));
    }

    #[test]
    fn an_unknown_stock_layout_is_refused() {
        assert!(patch_init("service recovery /wrong/path\n").is_err());
    }

    #[test]
    fn the_carrier_service_keeps_its_own_options() {
        let text = format!("{CARRIER_SERVICE}    socket recovery stream 422 system system\n");
        assert!(text.starts_with("service recovery /system/bin/skyward venus-helper probe\n"));
        assert!(text.contains("    oneshot\n"));
        assert!(text.contains("socket recovery"));
    }
}