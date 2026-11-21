//! RAM-only bring-up of the actual Skyward App and input pipeline.
//! This is NOT ui::run: no workers, BCB handling, mounts, credentials, USB
//! reconfiguration, settings persistence or system-effect executor start.
use crate::input::{Event, Inputs, monotonic_ms};
use anyhow::{Context, Result, ensure};
use skyward_config::DeviceConfig;
use skyward_ui::app::{Effect, SCREEN_OFF, Screen, Touch};
use skyward_ui::surface::kms::KmsSurface;
use skyward_ui::{App, Canvas, Fonts, Lang, Rgba, Surface, probe};
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

struct Backlight {
    old: String,
    last: u32,
    top: u32,
}
const BRIGHTNESS: &str = "/sys/class/backlight/panel0-backlight/brightness";
impl Backlight {
    fn open() -> Result<Self> {
        let old = fs::read_to_string(BRIGHTNESS).context("read original brightness")?;
        let last = old.trim().parse().context("parse brightness")?;
        let max: u32 = fs::read_to_string("/sys/class/backlight/panel0-backlight/max_brightness")?
            .trim()
            .parse()
            .context("parse maximum brightness")?;
        ensure!(max > 0, "empty backlight range");
        // Limit this test to the lower quarter of the raw range. Do not use
        // the production panel/HBM calibration before it has been verified.
        Ok(Self {
            old,
            last,
            top: (max / 4).clamp(1, 600),
        })
    }
    fn show(&mut self, value: Option<f32>) -> Result<()> {
        let raw = value.map_or(0, |x| ((x.clamp(0.0, 1.0) * self.top as f32) as u32).max(1));
        if raw != self.last {
            fs::write(BRIGHTNESS, raw.to_string())?;
            self.last = raw;
        }
        Ok(())
    }
}
impl Drop for Backlight {
    fn drop(&mut self) {
        if let Err(e) = fs::write(BRIGHTNESS, &self.old) {
            eprintln!("UI_PROBE_RESTORE_FAILED {e}");
        }
    }
}

fn effects(
    app: &mut App,
    effects: Vec<Effect>,
    now: u64,
    basic: &mut Option<crate::venus_basic::runtime::Basic>,
    fonts: &Fonts,
    canvas: &Canvas,
) {
    for effect in effects {
        if let Some(basic) = basic {
            basic.handle(app, effect, now, fonts, canvas);
            continue;
        }
        // Do not log values: some effects can hold credential text.
        if probe::refuse(app, effect, now) {
            eprintln!("UI_PROBE_EFFECT_REFUSED");
        }
    }
}

pub fn run(config: &Path, seconds: u64, until_stopped: bool, basic_features: bool) -> Result<()> {
    let autostart = if until_stopped {
        Some(
            fs::read_to_string("/skyward/ui-autostart.txt")
                .context("--until-stopped requires the UI-autostart image")?,
        )
    } else {
        None
    };
    let limit = probe::lifetime(seconds, until_stopped, autostart.as_deref())?;
    let manifest = fs::read_to_string("/skyward/probe-entryfix.txt")
        .context("ui-probe requires the temporary venus Recovery image")?;
    ensure!(
        manifest.starts_with("venus-entryfix-v1\n"),
        "unexpected Recovery probe"
    );
    let cfg = DeviceConfig::load(config)?;
    ensure!(
        cfg.device.codename == "venus",
        "only the verified venus target is supported"
    );
    ensure!(
        (160..=800).contains(&cfg.display.density),
        "configure a valid logical display density"
    );
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open("/tmp/venus-ui-probe.lock")?;
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another UI probe is already running"
    );
    STOP.store(false, Ordering::Relaxed);
    unsafe {
        ensure!(
            libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t) != libc::SIG_ERR,
            "install SIGTERM handler"
        );
        ensure!(
            libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t) != libc::SIG_ERR,
            "install SIGINT handler"
        );
    }
    fs::create_dir_all("/skyward/run/logs/ui-probe")?;
    let duration = limit.map_or_else(
        || "until-stopped".to_owned(),
        |d| format!("{}s", d.as_secs()),
    );
    eprintln!(
        "UI_PROBE_BEGIN: actual Skyward App; basic_features={basic_features}; duration={duration}"
    );
    let mut info = skyward_info::collect(&cfg).context("read device information")?;
    let fonts = Fonts::load("/skyward/MiSansVF.ttf", 0, "/skyward/KirkMono-Regular.ttf")?;
    let mut surface = KmsSurface::open("/dev/dri/card0")?;
    let (w, h) = surface.size();
    ensure!(
        (w, h) == (1440, 3200),
        "unexpected display geometry {w}x{h}"
    );
    info.panel = Some((w, h));
    eprintln!("UI_PROBE_DISPLAY {}", surface.describe());
    let mut inputs = Inputs::open(w, h, Vec::new())?;
    eprintln!("UI_PROBE_TOUCH_PRESENT={}", inputs.has_panel());
    let mut light = Backlight::open()?;
    let mut app = App::new(Screen::Home, w, h, cfg.display.density as f32 / 160.0);
    app.set_screen_corners(cfg.display.corner_radius as f32);
    app.take_settings(None, Some(Lang::Hans));
    app.state.screen_off = if until_stopped {
        SCREEN_OFF
            .iter()
            .position(|s| *s == Some(120_000))
            .context("missing two-minute idle setting")?
    } else {
        SCREEN_OFF.len() - 1
    };
    app.state.file_transfer = false;
    app.state.vibrate = false;
    app.state.brightness = 0.75;
    let mut basic = if basic_features {
        Some(crate::venus_basic::runtime::Basic::open(&cfg, &mut app)?)
    } else {
        None
    };
    let mut shown_clock = String::new();
    let mut canvas = Canvas::new(w, h);
    let began = Instant::now();
    let mut first = true;
    let mut last_screen = app.screen();
    let mut touches = 0u64;
    let mut moves = 0u64;
    let mut blanked = false;
    while limit.is_none_or(|duration| began.elapsed() < duration) && !STOP.load(Ordering::Relaxed) {
        let now = monotonic_ms();
        let tick = app.tick(now);
        effects(&mut app, tick, now, &mut basic, &fonts, &canvas);
        if let Some(basic) = &mut basic {
            basic.tick(&mut app, &mut info, now);
        }
        let clock = basic
            .as_ref()
            .map_or_else(|| "--:--".to_owned(), |b| b.clock(app.state.time_zone));
        if clock != shown_clock {
            shown_clock = clock;
            app.redraw();
        }
        let panel = app.panel(now);
        let mut drawn = false;
        if panel.is_some() && (app.take_dirty() || first || blanked) {
            app.compose(&mut canvas, &fonts, &info, &shown_clock, now);
            // Visible indication that these are real menus in an isolated
            // runtime, not a fully functional recovery installation.
            let y = h as f32 - 82.0;
            canvas.fill_rect(0.0, y, w as f32, 82.0, Rgba::rgb(26, 38, 58));
            canvas.text_center(
                &fonts.body,
                if basic_features {
                    "基础功能版 · 分区只读 · 暂不刷写 / 解密"
                } else {
                    "UI 测试 · 不执行刷写 / 清除 / 解密"
                },
                w as f32 / 2.0,
                y + 40.0,
                34.0,
                Rgba::rgb(240, 244, 250),
            );
            surface.present(&canvas)?;
            drawn = true;
            blanked = false;
            if first {
                surface.settle()?;
                fs::write("/skyward/run/logs/ui-probe/first-frame.rgba", &canvas.data)?;
                eprintln!(
                    "UI_PROBE_FIRST_FRAME_SETTLED {w}x{h} density={}",
                    cfg.display.density
                );
                let _ = skyward_sys::kmsg("VENUS_UI_FIRST_FRAME_SETTLED");
                if basic_features {
                    match crate::venus_basic::runtime::screenshot(&canvas) {
                        Ok(path) => eprintln!("VENUS_BASIC_FIRST_SCREENSHOT {}", path.display()),
                        Err(e) => eprintln!("VENUS_BASIC_SCREENSHOT_ERROR {e:#}"),
                    }
                }
                first = false;
            }
        } else if panel.is_none() && !blanked {
            canvas.data.fill(0);
            surface.present(&canvas)?;
            surface.settle()?;
            blanked = true;
        }
        light.show(panel)?;
        let deadline = app
            .deadline(now)
            .map(|t| t.saturating_sub(now).min(100) as i32)
            .unwrap_or(100);
        let wait = if drawn { 1 } else { deadline.clamp(1, 100) };
        let mut fd = libc::pollfd {
            fd: inputs.fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut fd, 1, wait) } < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e.into());
            }
        }
        for event in inputs.read()? {
            let now = monotonic_ms();
            let fx = match event {
                Event::Wake => Vec::new(), // The isolated UI probe never arms hardware gestures.
                Event::Touch(touch, at) => {
                    match touch {
                        Touch::Down(x, y) => {
                            touches += 1;
                            eprintln!("UI_PROBE_TOUCH_DOWN #{touches} {x:.1},{y:.1}");
                        }
                        Touch::Up => eprintln!("UI_PROBE_TOUCH_UP #{touches}"),
                        Touch::Move(_, _) => moves += 1,
                    }
                    app.touch(touch, &fonts, at)
                }
                Event::Key(key, down) => {
                    eprintln!("UI_PROBE_KEY {key:?} down={down}");
                    if down {
                        app.key(key, now)
                    } else {
                        app.key_up(key, now)
                    }
                }
            };
            effects(&mut app, fx, now, &mut basic, &fonts, &canvas);
        }
        if app.screen() != last_screen {
            eprintln!("UI_PROBE_PAGE {:?} -> {:?}", last_screen, app.screen());
            last_screen = app.screen();
        }
    }
    surface.settle()?;
    eprintln!("UI_PROBE_END touches={touches} moves={moves}; basic_features={basic_features}");
    Ok(())
}
