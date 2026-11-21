use crate::{common::*, runtime::*};
use anyhow::{Result, ensure};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    thread::sleep,
    time::Duration,
};
const PROBE: &str = "/skyward/run/logs/probe";
fn kmsg(message: &str) {
    if let Ok(mut f) = OpenOptions::new().write(true).open("/dev/kmsg") {
        let _ = writeln!(f, "<6>skyward-venus-probe: {message}");
    }
}
pub fn ui() -> Result<()> {
    let integrated = Path::new("/skyward/venus-recovery.txt").is_file();
    let base = Path::new(if integrated {
        "/skyward/run/state/recovery"
    } else {
        "/skyward/run/logs/ui-probe"
    });
    fs::create_dir_all(base)?;
    if !integrated && base.join("ui.log").is_file() {
        fs::rename(base.join("ui.log"), base.join("ui.previous.log"))?;
    }
    redirect(base.join("ui.log"), false)?;
    fs::write(base.join("pid"), format!("{}\n", std::process::id()))?;
    kmsg("VENUS_UI_AUTOSTART_WAIT v1");
    let ready = || {
        Path::new(PROBE).join("done").is_file()
            && fs::metadata("/dev/dri/card0").is_ok_and(|m| m.file_type().is_char_device())
    };
    for _ in 0..45 {
        if ready() {
            break;
        }
        sleep(Duration::from_secs(1));
    }
    ensure!(
        ready(),
        "VENUS_UI_AUTOSTART_NOT_READY: diagnostics/display unavailable; ADB remains independent"
    );
    let mut command = command(SKYWARD);
    if integrated {
        println!("VENUS_RECOVERY_UI_START data_mode=rw credential_input=on_device");
        command.args(["venus-recovery", "--config", "/skyward/device.toml"]);
    } else {
        println!("VENUS_UI_AUTOSTART_EXEC: isolated menu");
        command.args([
            "ui-probe",
            "--config",
            "/skyward/device.toml",
            "--until-stopped",
        ]);
        if Path::new("/skyward/basic-features.txt").is_file() {
            command.arg("--basic-features");
        }
    }
    exec(&mut command)
}
pub fn security() -> Result<()> {
    integrated()?;
    let dir = Path::new("/skyward/run/state/recovery");
    fs::create_dir_all(dir)?;
    redirect(dir.join("security.log"), true)?;
    checked("/skyward/runtime/rw-runtime", &[])?;
    println!("VENUS_SECURITY_SUPERVISOR_READY");
    // init reaps the entire service group when this owner exits. Remain
    // alive without restarting HALs after UI exits or Data becomes live.
    loop {
        sleep(Duration::from_secs(3600));
    }
}
pub fn probe() -> Result<()> {
    let base = Path::new(PROBE);
    fs::create_dir_all(base)?;
    kmsg("VENUS_PROBE_BEGIN entryfix-v1");
    redirect(base.join("probe.log"), false)?;
    println!("VENUS_PROBE_BEGIN entryfix-v1");
    for (src, name) in [
        ("/skyward/probe-entryfix.txt", "marker.txt"),
        ("/proc/cmdline", "cmdline.txt"),
        ("/proc/bootconfig", "bootconfig.txt"),
        ("/proc/mounts", "mounts.txt"),
        ("/proc/modules", "modules.txt"),
        ("/proc/bus/input/devices", "input-devices.txt"),
    ] {
        if let Ok(bytes) = fs::read(src) {
            let _ = fs::write(base.join(name), bytes);
        }
    }
    for (program, args, name) in [
        ("/system/bin/id", vec![], "id.txt"),
        ("/system/bin/uname", vec!["-a"], "uname.txt"),
        ("/system/bin/getprop", vec![], "properties.txt"),
        (
            "/system/bin/ls",
            vec![
                "-l",
                "/dev/dri",
                "/dev/input",
                "/sys/class/backlight",
                "/sys/class/udc",
            ],
            "nodes.txt",
        ),
        (SKYWARD, vec!["--version"], "skyward-version.txt"),
        (SKYWARD, vec!["--help"], "skyward-help.txt"),
    ] {
        let result = query(Path::new(program), &args, &base.join(name), 10);
        println!("{name}: {result:?}");
    }
    if let Ok(entries) = fs::read_dir("/dev/dri") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("card") {
                continue;
            }
            let card = entry.path();
            let result = query(
                Path::new(SKYWARD),
                &["kms", "--card", card.to_str().unwrap()],
                &base.join(format!("kms-{name}.txt")),
                10,
            );
            let m = format!("KMS_{name}={result:?}");
            println!("{m}");
            kmsg(&m);
        }
    }
    println!("VENUS_PROBE_END");
    kmsg("VENUS_PROBE_END");
    fs::write(base.join("done"), [])?;
    Ok(())
}
fn bounded_copy(source: &Path, destination: &Path, limit: u64) -> Result<()> {
    ensure!(
        !fs::symlink_metadata(destination).is_ok_and(|m| m.is_symlink()),
        "refusing symlink log destination"
    );
    let mut bytes = Vec::new();
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(source)?;
    ensure!(
        input.metadata()?.is_file(),
        "source log must be a regular file"
    );
    input.take(limit).read_to_end(&mut bytes)?;
    let temporary = destination.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    drop(file);
    fs::rename(temporary, destination)?;
    Ok(())
}
fn usb_snapshot() -> String {
    let controller = prop("sys.usb.controller").unwrap_or_default();
    let hardware = if !controller.is_empty()
        && controller
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        fs::read_to_string(format!("/sys/class/udc/{controller}/state"))
            .unwrap_or_default()
            .trim()
            .to_owned()
    } else {
        "unknown".into()
    };
    let bound = fs::read_to_string("/config/usb_gadget/g1/UDC").unwrap_or_default();
    let link = |n| {
        fs::read_link(format!("/config/usb_gadget/g1/configs/b.1/f{n}"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mut text = format!(
        "controller={controller} bound={} hardware={hardware} f1={} f2={}\n",
        bound.trim(),
        link(1),
        link(2)
    );
    for key in [
        "sys.usb.config",
        "sys.usb.state",
        "sys.usb.configfs",
        "sys.usb.config_disable",
        "sys.usb.ffs.ready",
        "sys.usb.ffs.mtp.ready",
        "init.svc.adbd",
        "init.svc.fastbootd",
    ] {
        text.push_str(&format!("{key}={}\n", prop(key).unwrap_or_default()));
    }
    text
}
pub fn logs() -> Result<()> {
    let base = Path::new(PROBE);
    fs::create_dir_all(base)?;
    kmsg("VENUS_PROBE_LOGGER_BEGIN entryfix-v1");
    let file = log(base.join("logcat-start.txt"), false)?;
    let mut logger = command("/system/bin/logcat")
        .args(["-b", "all", "-v", "threadtime", "-f"])
        .arg(base.join("logcat.txt"))
        .args(["-r", "1024", "-n", "2"])
        .stdout(file.try_clone()?)
        .stderr(file)
        .spawn()?;
    let result = (|| -> Result<()> {
        if !mounted("/metadata")? {
            for _ in 0..5 {
                let device = [
                    "/dev/block/by-name/metadata",
                    "/dev/block/bootdevice/by-name/metadata",
                ]
                .into_iter()
                .find(|p| fs::metadata(p).is_ok_and(|m| m.file_type().is_block_device()));
                if let Some(dev) = device {
                    let _ = query(
                        Path::new("/system/bin/mount"),
                        &["-t", "ext4", "-o", "nosuid,nodev,noatime", dev, "/metadata"],
                        &base.join("metadata-mount.txt"),
                        5,
                    );
                    break;
                }
                sleep(Duration::from_secs(1));
            }
        }
        let persistent = Path::new("/metadata/skyward-probe");
        let persist = mounted("/metadata")?
            && !fs::symlink_metadata(persistent).is_ok_and(|m| m.is_symlink())
            && fs::create_dir_all(persistent).is_ok()
            && fs::set_permissions(persistent, fs::Permissions::from_mode(0o700)).is_ok();
        if persist {
            for name in ["probe.log", "nodes.txt", "usb-state.txt"] {
                let _ = bounded_copy(
                    &persistent.join(format!("last-{name}")),
                    &persistent.join(format!("previous-{name}")),
                    131072,
                );
            }
            let _ = fs::write(
                persistent.join("last-start.txt"),
                "entryfix-v1 logger started\n",
            );
        }
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        let mut last_usb = String::new();
        let mut saved = false;
        loop {
            let usb = usb_snapshot();
            if usb != last_usb {
                let uptime = fs::read_to_string("/proc/uptime").unwrap_or_default();
                let seconds = uptime.split_whitespace().next().unwrap_or("unknown");
                fs::write(
                    base.join("usb-state.txt"),
                    format!("boot={} boot_seconds={seconds}\n{usb}", boot.trim()),
                )?;
                if persist {
                    let _ = bounded_copy(
                        &base.join("usb-state.txt"),
                        &persistent.join("last-usb-state.txt"),
                        32768,
                    );
                }
                kmsg(&format!("VENUS_USB_STATE boot_seconds={seconds} {usb}"));
                last_usb = usb;
            }
            if query(
                Path::new("/system/bin/dmesg"),
                &[],
                &base.join("kernel.tmp"),
                5,
            )
            .unwrap_or(false)
            {
                let _ = fs::rename(base.join("kernel.tmp"), base.join("kernel.txt"));
            }
            if !saved && persist && base.join("done").is_file() {
                let logs = bounded_copy(
                    &base.join("probe.log"),
                    &persistent.join("last-probe.log"),
                    131072,
                );
                let nodes = bounded_copy(
                    &base.join("nodes.txt"),
                    &persistent.join("last-nodes.txt"),
                    32768,
                );
                if logs.is_ok() && nodes.is_ok() {
                    unsafe {
                        libc::sync();
                    }
                    saved = true;
                    kmsg("PROBE_SUMMARY_SAVED");
                }
            }
            sleep(Duration::from_secs(2));
        }
    })();
    let _ = logger.kill();
    let _ = logger.wait();
    result
}
