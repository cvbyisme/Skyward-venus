//! The functions the USB port offers, which init sets up as
//! `sys.usb.config` asks: adb while the recovery waits, with files over MTP
//! beside it while file transfer is on.

use crate::{prop, set_prop};
use anyhow::{ensure, Result};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static MTP: AtomicBool = AtomicBool::new(false);

/// What the port offers while nothing else has it.
pub fn resting() -> &'static str {
    if MTP.load(Ordering::Relaxed) {
        "mtp,adb"
    } else {
        "adb"
    }
}

/// Whether MTP goes beside adb while the port rests: from when its function
/// has its descriptors until it goes.
pub fn set_mtp(on: bool) {
    MTP.store(on, Ordering::Relaxed);
}

/// Configfs state is the kernel's acknowledgement. An init property can
/// already say adb even when writing UDC failed. A disconnected cable is
/// valid here: the local gadget must be bound, not host-configured.
fn configfs_ready(root: &Path, controller: &str, config: &str) -> bool {
    let expected: &[&str] = match config {
        "none" => &[],
        "adb" | "sideload" => &["ffs.adb"],
        "fastboot" => &["ffs.fastboot"],
        "mtp,adb" => &["ffs.mtp", "ffs.adb"],
        _ => return true, // Other device profiles keep their existing rules.
    };
    let Ok(bound) = fs::read_to_string(root.join("UDC")) else { return false };
    if config == "none" { return bound.trim().is_empty() }
    if controller.is_empty() || !controller.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)) || bound.trim() != controller {
        return false;
    }
    let links = root.join("configs/b.1");
    for (index, name) in expected.iter().enumerate() {
        let at=links.join(format!("f{}",index+1));
        if fs::read_link(&at).is_err() { return false }
        // Configfs exposes relative links even when init supplied an
        // absolute target. Compare resolved paths, not the spelling.
        let Ok(actual)=fs::canonicalize(at) else { return false };
        let Ok(want)=fs::canonicalize(root.join("functions").join(name)) else { return false };
        if actual!=want { return false }
    }
    let Ok(entries) = fs::read_dir(links) else { return false };
    // A stale MTP/fastboot function must not make an adb-only request pass.
    let actual = entries.filter_map(|e| e.ok()).filter(|e| e.file_type().is_ok_and(|t| t.is_symlink())).count();
    actual == expected.len()
}

fn producers_ready(config: &str, adbd: &str, ffs: &str, mtp: &str) -> bool {
    match config {
        "adb" => adbd=="running" && ffs=="1",
        "mtp,adb" => adbd=="running" && ffs=="1" && mtp=="1",
        // Sideload owns FunctionFS in the installer, after stopping adbd.
        "sideload" | "fastboot" => ffs=="1",
        _ => true,
    }
}

fn ready(config: &str) -> bool {
    if prop("sys.usb.state").as_deref() != Some(config) { return false }
    if prop("sys.usb.configfs").as_deref() != Some("1") { return true }
    if !producers_ready(config,&prop("init.svc.adbd").unwrap_or_default(),
        &prop("sys.usb.ffs.ready").unwrap_or_default(),&prop("sys.usb.ffs.mtp.ready").unwrap_or_default()) { return false }
    configfs_ready(Path::new("/config/usb_gadget/g1"),&prop("sys.usb.controller").unwrap_or_default(),config)
}

/// Ask init for `config` and wait until it is in place. Going to "none"
/// also waits for adbd to be gone, so its function is free.
pub fn set(config: &str, timeout: Duration) -> Result<()> {
    set_prop("sys.usb.config", config)?;
    let until = Instant::now()+timeout;
    while !ready(config) {
        ensure!(Instant::now()<until,"USB configuration {config} was not acknowledged by the gadget: state={:?} controller={:?} bound={:?}",
            prop("sys.usb.state"),prop("sys.usb.controller"),fs::read_to_string("/config/usb_gadget/g1/UDC").ok());
        thread::sleep(Duration::from_millis(10));
    }
    if config == "none" {
        let start = Instant::now();
        while !matches!(prop("init.svc.adbd").as_deref(), None | Some("stopped")) {
            ensure!(start.elapsed() < timeout, "adbd still runs {} ms after the port went to none", timeout.as_millis());
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

/// Hand the port to `config` through "none", which stops the programs
/// serving it and takes every function off; init then sets the port up
/// once the programs behind the new configuration are ready.
pub fn switch(config: &str, timeout: Duration) -> Result<()> {
    set("none", timeout)?;
    set(config, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn gadget() -> tempfile::TempDir {
        let dir=tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("configs/b.1")).unwrap();
        for function in ["ffs.adb","ffs.mtp","ffs.fastboot"] {
            fs::create_dir_all(dir.path().join("functions").join(function)).unwrap();
        }
        dir
    }
    #[test]
    fn an_unbound_gadget_cannot_acknowledge_adb() {
        let d=gadget();fs::write(d.path().join("UDC"),"\n").unwrap();
        symlink(d.path().join("functions/ffs.adb"),d.path().join("configs/b.1/f1")).unwrap();
        assert!(!configfs_ready(d.path(),"a600000.dwc3","adb"));
        assert!(configfs_ready(d.path(),"a600000.dwc3","none"));
        fs::write(d.path().join("UDC"),"a600000.dwc3\n").unwrap();
        assert!(configfs_ready(d.path(),"a600000.dwc3","adb"));
        assert!(!configfs_ready(d.path(),"dummy_udc.0","adb"));
    }
    #[test]
    fn stale_or_missing_functions_do_not_acknowledge_the_requested_mode() {
        let d=gadget();fs::write(d.path().join("UDC"),"a600000.dwc3\n").unwrap();
        let links=d.path().join("configs/b.1");
        symlink(d.path().join("functions/ffs.mtp"),links.join("f1")).unwrap();
        assert!(!configfs_ready(d.path(),"a600000.dwc3","adb"));
        assert!(!configfs_ready(d.path(),"a600000.dwc3","mtp,adb"));
        symlink(d.path().join("functions/ffs.adb"),links.join("f2")).unwrap();
        assert!(configfs_ready(d.path(),"a600000.dwc3","mtp,adb"));
        fs::remove_file(links.join("f1")).unwrap();
        symlink(d.path().join("functions/ffs.adb"),links.join("f1")).unwrap();
        assert!(!configfs_ready(d.path(),"a600000.dwc3","adb"));
    }
    #[test]
    fn configfs_relative_links_resolve_to_the_correct_function() {
        let d=gadget();fs::write(d.path().join("UDC"),"a600000.dwc3\n").unwrap();
        symlink("../../functions/ffs.adb",d.path().join("configs/b.1/f1")).unwrap();
        assert!(configfs_ready(d.path(),"a600000.dwc3","adb"));
        assert!(!configfs_ready(d.path(),"a600000.dwc3","fastboot"));
    }
    #[test]
    fn sideload_keeps_adbd_stopped_while_the_installer_owns_functionfs() {
        assert!(producers_ready("sideload","stopped","1","0"));
        assert!(!producers_ready("sideload","stopped","0","0"));
        assert!(!producers_ready("adb","stopped","1","0"));
        assert!(!producers_ready("mtp,adb","running","1","0"));
        assert!(producers_ready("mtp,adb","running","1","1"));
    }
}
