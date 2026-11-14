use crate::ramdisk::Ramdisk;
use anyhow::{ensure, Context, Result};

const INIT: &str = "system/etc/init/hw/init.rc";

fn patch_init(input: &str) -> Result<String> {
    let service = "service recovery /system/bin/recovery\n";
    ensure!(input.matches(service).count() == 1, "unexpected recovery service");
    let factory = "import /miui.factoryreset.rc\n";
    ensure!(input.matches(factory).count() == 1, "unexpected factory-reset import");
    let mut text = input.replacen(factory, "# Factory-reset hooks disabled for venus probe.\n", 1);
    text = text.replacen(
        service,
        "service recovery /system/bin/sh /skyward/probe.sh\n    disabled\n    oneshot\n",
        1,
    );
    text.push_str(include_str!("venus_probe.rc"));
    Ok(text)
}

pub fn prepare(rd: &mut Ramdisk) -> Result<()> {
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
    rd.add("skyward/probe.sh", 0o755, include_bytes!("venus_probe.sh").to_vec())?;
    rd.add("skyward/probe-logs.sh", 0o755, include_bytes!("venus_probe_logs.sh").to_vec())?;
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
        assert!(out.contains("service recovery /system/bin/sh /skyward/probe.sh"));
        assert!(out.contains("setprop sys.usb.config adb"));
        assert!(out.contains("    disabled\n    oneshot\n"));
        assert!(out.contains("    seclabel u:r:recovery:s0\n"));
    }
    #[test]
    fn unknown_layout_is_refused() {
        assert!(patch_init("service recovery /wrong/path\n").is_err());
    }
}
