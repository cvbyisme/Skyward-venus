//! Own Venus's installed WLAN helpers while the official WLAN worker runs.
//! Helpers see only the verified RAM runtime and volatile Wi-Fi state.
use anyhow::{ensure, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub struct Runtime {
    children: Vec<Child>,
    _lock: File,
    powered: bool,
}

/// The UI can start before its asynchronous Data/security setup has
/// finished caching the installed runtime. Readiness only schedules work;
/// Runtime::start still verifies all six live services before launching.
pub fn ready() -> bool {
    std::path::Path::new("/tmp/skyward-recovery/runtime-ready").is_file()
        && skyward_sys::prop("init.svc.venus_security").as_deref() == Some("running")
}

impl Runtime {
    pub fn start() -> Result<Self> {
        crate::venus_runtime::verified()?;
        let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/tmp/skyward-wlan.lock")?;
        ensure!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "another Venus WLAN controller is running");
        crate::venus_wlan_mac::prepare()?;
        let mut runtime = Self { children: Vec::new(), _lock: lock, powered: false };
        for role in ["vndmanager", "peripheral", "cnss"] {
            let log = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(format!("/tmp/skyward-wlan-{role}.log"))?;
            let child = Command::new("/skyward/runtime/km41-launch").arg(role)
                .stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).spawn()
                .with_context(|| format!("start the installed WLAN helper {role}"))?;
            runtime.children.push(child);
            thread::sleep(Duration::from_millis(150));
            runtime.check()?;
        }
        // The built-in Venus driver copies exactly three bytes from this
        // character device, including for ON. An empty/two-byte write is
        // not a valid control operation.
        fs::write("/sys/devices/platform/soc/b0000000.qcom,cnss-qca6490/fs_ready", b"1\n")?;
        runtime.powered = true;
        fs::write("/dev/wlan", b"ON\n")?;
        runtime.check()?;
        Ok(runtime)
    }

    pub fn check(&mut self) -> Result<()> {
        for child in &mut self.children {
            ensure!(child.try_wait()?.is_none(), "an installed WLAN helper ended; see /tmp/skyward-wlan-*.log");
        }
        Ok(())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if self.powered {
            if let Err(error) = fs::write("/dev/wlan", b"OFF") {
                let _ = skyward_sys::kmsg(&format!("Venus WLAN power-off notification: {error}"));
            }
        }
        for child in self.children.iter_mut().rev() {
            if matches!(child.try_wait(), Ok(Some(_))) { continue; }
            unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
            let until = Instant::now() + Duration::from_secs(3);
            while Instant::now() < until {
                if matches!(child.try_wait(), Ok(Some(_))) { break; }
                thread::sleep(Duration::from_millis(50));
            }
            if matches!(child.try_wait(), Ok(None)) { let _ = child.kill(); }
            let _ = child.wait();
        }
    }
}
