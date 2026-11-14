//! Bounded display-only test for the already-booted venus entryfix probe.
//! Never starts recovery(), the UI effect executor, decryption, services,
//! BCB processing or any block-device operation. No input events are accepted.
//! Only the DRM display and (temporarily) backlight are changed.
use skyward_ui::{Canvas, Rgba};

fn pattern(w: u32, h: u32) -> Canvas {
    let mut canvas = Canvas::filled(w, h, Rgba::rgb(18, 20, 26));
    let margin = w as f32 * 0.07;
    let width = w as f32 - 2.0 * margin;
    for (i, color) in [
        Rgba::rgb(230, 45, 45),
        Rgba::rgb(45, 210, 100),
        Rgba::rgb(45, 115, 235),
        Rgba::rgb(240, 240, 240),
    ]
    .into_iter()
    .enumerate()
    {
        canvas.fill_rect(
            margin + width * i as f32 / 4.0,
            h as f32 * 0.58,
            width / 4.0,
            h as f32 * 0.16,
            color,
        );
    }
    canvas
}

#[cfg(target_os = "android")]
fn main() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use skyward_ui::surface::kms::KmsSurface;
    use skyward_ui::{Fonts, Surface};
    use std::{fs, path::PathBuf, time::Duration};

    let seconds: u64 = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "15".into())
        .parse()
        .context("duration must be a number of seconds")?;
    ensure!(
        (1..=60).contains(&seconds),
        "duration must be 1..=60 seconds"
    );
    let manifest = fs::read_to_string("/skyward/probe-entryfix.txt")
        .context("run only inside the temporary venus entryfix Recovery")?;
    ensure!(
        manifest.starts_with("venus-entryfix-v1\n"),
        "unknown probe image"
    );

    let fonts = Fonts::load("/skyward/MiSansVF.ttf", 0, "/skyward/KirkMono-Regular.ttf")?;
    eprintln!("PANEL_PROBE_BEGIN: display only, no touch/actions/block devices");
    let mut surface = KmsSurface::open("/dev/dri/card0").context("open Skyward KMS backend")?;
    let (w, h) = surface.size();
    eprintln!("PANEL_OPEN: {}", surface.describe());
    let mut canvas = pattern(w, h);
    let x = w as f32 * 0.07;
    let white = Rgba::rgb(242, 244, 248);
    let gray = Rgba::rgb(170, 180, 196);
    canvas.text_top(
        &fonts.title,
        "Skyward",
        x,
        h as f32 * 0.17,
        w as f32 * 0.095,
        white,
        None,
    );
    canvas.text_top(
        &fonts.heading,
        "venus / DISPLAY PROBE",
        x,
        h as f32 * 0.29,
        w as f32 * 0.045,
        white,
        None,
    );
    canvas.text_top(
        &fonts.body,
        "ADB remains available",
        x,
        h as f32 * 0.38,
        w as f32 * 0.037,
        gray,
        None,
    );
    canvas.text_top(
        &fonts.body,
        "No flash / no data operations",
        x,
        h as f32 * 0.44,
        w as f32 * 0.037,
        gray,
        None,
    );
    canvas.text_top(
        &fonts.body,
        "Red   Green   Blue   White",
        x,
        h as f32 * 0.78,
        w as f32 * 0.035,
        gray,
        None,
    );

    // Restore brightness on normal return or an error. A watchdog outside
    // this process is required for a driver ioctl that fails to return.
    struct Backlight {
        path: PathBuf,
        old: String,
    }
    impl Drop for Backlight {
        fn drop(&mut self) {
            if let Err(e) = fs::write(&self.path, &self.old) {
                eprintln!("BACKLIGHT_RESTORE_FAILED: {e}");
            }
        }
    }
    let path = PathBuf::from("/sys/class/backlight/panel0-backlight/brightness");
    let old = fs::read_to_string(&path).context("read original brightness")?;
    let maximum: u32 = fs::read_to_string("/sys/class/backlight/panel0-backlight/max_brightness")?
        .trim()
        .parse()?;
    ensure!(maximum > 0, "invalid maximum brightness");
    let level = (maximum / 5).clamp(1, 400);
    let backlight = Backlight { path, old };
    fs::write(&backlight.path, level.to_string()).context("set test brightness")?;
    eprintln!("BACKLIGHT: {} -> {level}", backlight.old.trim());

    surface
        .present(&canvas)
        .context("present diagnostic frame")?;
    surface.settle().context("wait for diagnostic frame flip")?;
    eprintln!("PANEL_FRAME_SETTLED: {w}x{h}; hold {seconds}s; no input accepted");
    std::thread::sleep(Duration::from_secs(seconds));
    drop(backlight);
    drop(surface);
    eprintln!("PANEL_PROBE_END: brightness restored; visual confirmation still required");
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn main() {
    let _ = pattern(16, 32);
    eprintln!(
        "Build this example for aarch64-linux-android; it is a temporary device display test."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_frame_is_opaque_and_full_size() {
        let canvas = pattern(100, 200);
        assert_eq!(canvas.data.len(), 100 * 200 * 4);
        assert!(canvas.data.chunks_exact(4).all(|p| p[3] == 255));
        assert_eq!(&canvas.data[..4], &[18, 20, 26, 255]);
    }

    #[test]
    fn diagnostic_bars_have_the_expected_channel_order() {
        let canvas = pattern(100, 200);
        for (x, expected) in [
            (15, [230, 45, 45, 255]),
            (38, [45, 210, 100, 255]),
            (60, [45, 115, 235, 255]),
            (82, [240, 240, 240, 255]),
        ] {
            let at = (130 * 100 + x) * 4;
            assert_eq!(&canvas.data[at..at + 4], &expected);
        }
    }
}
