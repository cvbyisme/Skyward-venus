//! Explicit, bounded AOD lifecycle experiment. Run only after the parent has
//! stopped the UI owning DRM. Uses DRM LP1/LP2 and native doze brightness;
//! never writes raw DSI commands or requests system suspend.
//! Usage: venus_aod_probe --run SECONDS [DOZE_LEVEL=2] [--baseline]
use skyward_ui::{Canvas, Rgba};

fn face(w: u32, h: u32, second: bool) -> Canvas {
    let mut canvas = Canvas::filled(w, h, Rgba::rgb(0, 0, 0));
    let x = w as f32 * if second { 0.58 } else { 0.28 };
    // Sparse, dim pixels make the two settled frames visually distinguishable.
    canvas.fill_rect(x, h as f32 * 0.32, w as f32 * 0.14, h as f32 * 0.025,
                     Rgba::rgb(96, 96, 96));
    canvas.fill_rect(x, h as f32 * 0.36, w as f32 * 0.07, h as f32 * 0.025,
                     Rgba::rgb(96, 96, 96));
    canvas
}

fn normal_baseline(w: u32, h: u32) -> Canvas {
    let mut canvas = Canvas::filled(w, h, Rgba::rgb(24, 35, 48));
    canvas.fill_rect(w as f32 * 0.3, h as f32 * 0.4, w as f32 * 0.4, h as f32 * 0.05,
                     Rgba::rgb(180, 190, 200));
    canvas
}

#[cfg(target_os = "android")]
mod device {
    use super::*;
    use anyhow::{Context, Result, bail, ensure};
    use skyward_sys::alarm::{AlarmEvent, WakeAlarm};
    use skyward_ui::Surface;
    use skyward_ui::surface::kms::{self, KmsSurface};
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    const BRIGHTNESS: &str = "/sys/class/backlight/panel0-backlight/brightness";
    const DOZE: &str = "/sys/class/mi_display/disp-DSI-0/doze_brightness";
    static STOP: AtomicBool = AtomicBool::new(false);
    extern "C" fn stop(_: libc::c_int) { STOP.store(true, Ordering::Relaxed); }

    struct Restore {
        surface: KmsSurface,
        frame: Canvas,
        brightness: String,
        baseline: bool,
        restored: bool,
    }
    impl Restore {
        fn finish(&mut self) -> Result<()> {
            self.surface.wake().context("restore normal ON/NOLP")?;
            fs::write(DOZE, "0\n").context("clear native doze brightness")?;
            self.surface.present(&self.frame).context("restore original frame")?;
            self.surface.settle()?;
            fs::write(BRIGHTNESS, &self.brightness).context("restore original normal brightness")?;
            self.restored = true;
            eprintln!("AOD_PROBE_RESTORED requested_lp={:?} brightness={} frame={}",
                      self.surface.requested_lp()?, self.brightness.trim(),
                      if self.baseline { "generated-normal-baseline" } else { "captured-original" });
            Ok(())
        }
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            if !self.restored {
                if let Err(error) = self.finish() {
                    eprintln!("AOD_PROBE_RESTORE_FAILED {error:#}");
                }
            }
        }
    }

    fn verify_doze(expected: u32) -> Result<()> {
        let held: u32 = fs::read_to_string(DOZE)?.trim().parse().context("parse native doze brightness")?;
        ensure!(held == expected, "native doze brightness is {held}, requested {expected}");
        Ok(())
    }

    fn wait_alarm(alarm: &WakeAlarm, seconds: u64) -> Result<()> {
        let due = alarm.arm_after(Duration::from_secs(seconds)).context("arm genuine wake alarm")?;
        eprintln!("AOD_ALARM_ARMED unix_ms={due}; system suspend is not requested by this probe");
        let deadline = Instant::now() + Duration::from_secs(seconds + 2);
        loop {
            ensure!(!STOP.load(Ordering::Relaxed), "probe interrupted; restoring display");
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "wake alarm did not expire within its deadline");
            let mut fd = libc::pollfd { fd: alarm.fd(), events: libc::POLLIN, revents: 0 };
            let result = unsafe { libc::poll(&mut fd, 1, remaining.as_millis().min(1000) as i32) };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted { continue; }
                return Err(error).context("wait for wake alarm");
            }
            ensure!(fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) == 0, "wake alarm fd failed");
            match alarm.drain()? {
                AlarmEvent::Pending => {}
                AlarmEvent::ClockChanged => bail!("wall clock changed; probe requires a stable interval"),
                AlarmEvent::Fired(count) => {
                    eprintln!("AOD_ALARM_FIRED count={count}");
                    return Ok(());
                }
            }
        }
    }

    pub fn run() -> Result<()> {
        let mut args: Vec<_> = std::env::args().collect();
        let baseline = args.last().is_some_and(|arg| arg == "--baseline");
        if baseline { args.pop(); }
        if args.len() < 3 || args[1] != "--run" {
            bail!("usage: venus_aod_probe --run SECONDS(2..30) [DOZE_LEVEL(1|2)] [--baseline]; UI must release DRM first");
        }
        ensure!(args.len() <= 4, "too many arguments");
        let seconds: u64 = args[2].parse()?;
        let level: u32 = args.get(3).map(String::as_str).unwrap_or("2").parse()?;
        ensure!((2..=30).contains(&seconds) && (1..=2).contains(&level), "invalid probe duration or doze level");
        // Fail before taking DRM or modifying the panel when real wake alarms
        // are unavailable. No fallback to a timer that cannot wake the phone.
        let alarm = WakeAlarm::new().context("create CLOCK_REALTIME_ALARM")?;
        verify_doze(0).context("start this experiment from a normal lit display")?;
        let brightness = fs::read_to_string(BRIGHTNESS)?;
        ensure!(brightness.trim().parse::<u32>()? > 0, "start from a normal nonzero brightness");
        let frame = if baseline { None } else {
            Some(kms::capture("/dev/dri/card0").context("capture original frame before taking display ownership; when the stopped UI disabled its connector, explicitly use --baseline")?)
        };
        let surface = KmsSurface::open("/dev/dri/card0")?;
        let (w, h) = surface.size();
        let frame = frame.unwrap_or_else(|| normal_baseline(w, h));
        let mut restore = Restore { surface, frame, brightness, baseline, restored: false };
        ensure!(restore.surface.supports_aod(), "connector does not support LP1/LP2");
        ensure!((restore.frame.w, restore.frame.h) == (w, h), "captured frame size differs from KMS preferred mode");
        unsafe {
            libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
        }
        let tested = (|| -> Result<()> {
            // A stopped UI may leave no encoder/active CRTC. Establish a
            // known normal state before testing doze, without inventing an
            // original screenshot when --baseline was explicitly selected.
            restore.surface.present(&restore.frame)?;
            restore.surface.settle()?;
            fs::write(BRIGHTNESS, &restore.brightness)?;
            if baseline {
                eprintln!("AOD_PROBE_BASELINE initialized normal display; no original UI frame was captured");
            }
            restore.surface.present(&face(w, h, false))?;
            restore.surface.settle()?;
            // Ordinary brightness is lowered before entering LP1 and before
            // native doze brightness. It is never written while in AOD.
            fs::write(BRIGHTNESS, "0\n")?;
            restore.surface.begin_aod()?;
            fs::write(DOZE, format!("{level}\n"))?;
            verify_doze(level)?;
            restore.surface.sleep_aod()?;
            ensure!(restore.surface.is_aod_sleeping(), "initial frame did not reach AOD LP2");
            eprintln!("AOD_LP2_FIRST requested_lp={:?} native_doze={level}; visual confirmation required",
                      restore.surface.requested_lp()?);
            wait_alarm(&alarm, seconds / 2)?;
            restore.surface.present_aod(&face(w, h, true))?;
            verify_doze(level)?;
            ensure!(restore.surface.is_aod_sleeping(), "second frame did not return to AOD LP2");
            eprintln!("AOD_LP2_SECOND requested_lp={:?}; update used LP1, never ON/NOLP",
                      restore.surface.requested_lp()?);
            wait_alarm(&alarm, seconds - seconds / 2)?;
            Ok(())
        })();
        let disarmed = alarm.disarm();
        let restored = restore.finish();
        tested?;
        disarmed?;
        restored?;
        eprintln!("AOD_PROBE_PASS_REQUESTS; physical brightness and suspend-wake behavior require device observation");
        Ok(())
    }
}

#[cfg(target_os = "android")]
fn main() -> anyhow::Result<()> { device::run() }

#[cfg(not(target_os = "android"))]
fn main() {
    let _ = face(32, 64, false);
    let _ = normal_baseline(32, 64);
    eprintln!("Build for aarch64-linux-android; device execution is an explicit display experiment.");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn probe_frames_are_sparse_and_distinguishable() {
        let first = face(100, 200, false);
        let second = face(100, 200, true);
        assert_ne!(first.data, second.data);
        assert!(first.data.chunks_exact(4).all(|p| p[3] == 255));
        assert!(first.data.chunks_exact(4).filter(|p| p[0] != 0).count() < 100 * 200 / 50);
    }

    #[test]
    fn normal_baseline_is_opaque_and_distinct_from_both_doze_frames() {
        let baseline = normal_baseline(100, 200);
        assert_ne!(baseline.data, face(100, 200, false).data);
        assert_ne!(baseline.data, face(100, 200, true).data);
        assert!(baseline.data.chunks_exact(4).all(|p| p[3] == 255 && p[0] != 0));
    }
}
