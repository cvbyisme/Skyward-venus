//! The phone's readings taken on a thread of their own, so the screen keeps
//! up: the battery's driver takes milliseconds to answer each read.

use anyhow::{Context, Result};
use skyward_config::DeviceConfig;
use skyward_info::{Battery, DeviceInfo};
use skyward_sys::worker::{self, Told};
use std::os::fd::RawFd;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// The battery is read this often, and at once when asked; all the
/// readings every so many times instead.
const BATTERY_EVERY: Duration = Duration::from_secs(1);
const ALL_EVERY: Duration = Duration::from_secs(30);

pub enum Reading {
    /// The battery, and when reading it started.
    Battery(Result<Option<Battery>>, Instant),
    /// All the readings, and when taking them started.
    All(Box<Result<DeviceInfo>>, Instant),
}

pub struct Readings {
    taken: Told<Reading>,
    asks: Sender<bool>,
}

impl Readings {
    /// Start taking the readings of the phone `cfg` describes, the first a
    /// second from now. The thread ends once the screen lets them go.
    pub fn start(cfg: &DeviceConfig) -> Result<Readings> {
        let (tell, taken) = worker::telling().context("make the readings' wake pipe")?;
        let (asks, asked) = mpsc::channel::<bool>();
        let cfg = cfg.clone();
        thread::Builder::new()
            .name("readings".into())
            .spawn(move || {
                let mut all_at = Instant::now();
                loop {
                    let full = match asked.recv_timeout(BATTERY_EVERY) {
                        Ok(full) => asked.try_iter().fold(full, |a, b| a || b),
                        Err(RecvTimeoutError::Timeout) => false,
                        Err(RecvTimeoutError::Disconnected) => return,
                    };
                    let at = Instant::now();
                    // Healthd may produce events faster than the timeout.
                    // A wall-clock deadline cannot be starved by that stream.
                    let reading = if full || at.duration_since(all_at) >= ALL_EVERY {
                        all_at = at;
                        Reading::All(Box::new(crate::ui::collect_info(&cfg)), at)
                    } else {
                        Reading::Battery(skyward_info::battery_now(), at)
                    };
                    if !tell.tell(reading) {
                        return;
                    }
                }
            })
            .context("start the readings thread")?;
        Ok(Readings { taken, asks })
    }

    /// Readable while readings wait.
    pub fn fd(&self) -> RawFd {
        self.taken.fd()
    }

    /// Read the battery at once.
    pub fn ask(&self) {
        self.asks.send(false).expect("the readings thread runs while its readings are taken");
    }

    /// Refresh storage and installed-system information after mounting Data.
    pub fn refresh(&self) {
        self.asks.send(true).expect("the readings thread runs while its readings are taken");
    }

    /// The readings taken since the last call.
    pub fn taken(&self) -> Vec<Reading> {
        self.taken.take()
    }
}
