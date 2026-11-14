//! Read-only modem readings on their own thread; a slow/unavailable modem
//! must not stall touch, battery readings or decryption.
use anyhow::{Context, Result};
use skyward_net::mobile::Mobile;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const POLL: Duration = Duration::from_secs(5);

pub struct Worker {
    latest: Arc<Mutex<Option<Mobile>>>,
    ask: SyncSender<()>,
    stopped: Arc<AtomicBool>,
    wake: Arc<OwnedFd>,
}

/// Query failures are reported separately from an empty or absent SIM.
/// Merely reading the state never starts the modem or changes its power.
fn reading(owner_requested: &mut bool) -> Mobile {
    use skyward_net::modem::Phase;
    if !*owner_requested {
        if !crate::venus_wlan::ready() { return Mobile { checking:true,..Mobile::default() }; }
        if let Err(error) = crate::venus_modem::ensure_owner() {
            return Mobile { incomplete:true,issue:Some(format!("start mobile network: {error:#}")),..Mobile::default() };
        }
        *owner_requested = true;
    }
    let snapshot = match crate::venus_modem::snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => return Mobile { incomplete:true,issue:Some(format!("read mobile network: {error:#}")),..Mobile::default() },
    };
    let Some(modem) = snapshot else { return Mobile { checking:true,..Mobile::default() }; };
    // A stopped or starting owner is not a failed SIM query. Its independent
    // state is sufficient until it has brought the radio all the way up.
    if modem.radio != Phase::Ready || matches!(modem.data,Phase::Starting|Phase::Stopping) {
        return Mobile { checking:modem.radio == Phase::Starting,
            modem_started:Some(modem.radio != Phase::Off),modem:Some(modem),..Mobile::default() };
    }
    let mut mobile = skyward_net::cellular::mobile().unwrap_or_else(|error| Mobile {
        incomplete:true,issue:Some(format!("{error:#}")),..Mobile::default()
    });
    mobile.modem_started = Some(true);
    // A stop requested during the bounded QMI reads must not redisplay the
    // old registration or retain DNS from a data call already torn down.
    match crate::venus_modem::snapshot() {
        Ok(Some(current)) if current.radio != Phase::Ready => Mobile {
            checking:current.radio == Phase::Starting,modem_started:Some(current.radio != Phase::Off),
            modem:Some(current),..Mobile::default()
        },
        Ok(Some(current)) => { mobile.modem=Some(current); mobile },
        Ok(None) => Mobile { checking:true,..Mobile::default() },
        Err(error) => Mobile { incomplete:true,issue:Some(format!("read mobile network: {error:#}")),..Mobile::default() },
    }
}

impl Worker {
    pub fn start() -> Result<Self> {
        let mut owner_requested = false;
        Self::spawn(move || reading(&mut owner_requested), POLL)
    }

    fn spawn(mut query: impl FnMut() -> Mobile + Send + 'static, interval: Duration) -> Result<Self> {
        let mut fds = [0; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return Err(std::io::Error::last_os_error()).context("make the SIM readings pipe");
        }
        let (wake, signal) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let wake = Arc::new(wake);
        let latest = Arc::new(Mutex::new(None));
        let stopped = Arc::new(AtomicBool::new(false));
        // A refresh while a query is running requests exactly one follow-up.
        // Holding a key or revisiting the page cannot build an unbounded queue.
        let (ask, asked) = mpsc::sync_channel(1);
        let (out, end, reader) = (latest.clone(), stopped.clone(), wake.clone());
        thread::Builder::new().name("sim-readings".into()).spawn(move || {
            // Keep the read end alive until the last possible wake write, even
            // when the UI drops the worker in the middle of a bounded query.
            let _reader = reader;
            let mut previous_issue = None;
            while !end.load(Ordering::Acquire) {
                let mobile = query();
                if end.load(Ordering::Acquire) { return; }
                if mobile.issue != previous_issue {
                    if let Some(issue) = &mobile.issue {
                        let _ = skyward_sys::kmsg(&format!("SIM readings incomplete: {issue}"));
                    } else if previous_issue.is_some() {
                        let _ = skyward_sys::kmsg("SIM readings recovered");
                    }
                    previous_issue = mobile.issue.clone();
                }
                // If the screen has been busy, retain the newest complete
                // snapshot rather than all stale intermediate reports.
                *out.lock().unwrap_or_else(|p| p.into_inner()) = Some(mobile);
                unsafe { libc::write(signal.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
                match asked.recv_timeout(interval) {
                    Err(RecvTimeoutError::Disconnected) => return,
                    Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                }
            }
        }).context("start SIM readings")?;
        Ok(Self { latest, ask, stopped, wake })
    }

    pub fn fd(&self) -> RawFd { self.wake.as_raw_fd() }
    pub fn refresh(&self) { let _ = self.ask.try_send(()); }
    pub fn taken(&self) -> Option<Mobile> {
        let mut bytes = [0u8; 64];
        while unsafe { libc::read(self.wake.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) } > 0 {}
        self.latest.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.ask.try_send(());
        // Each QRTR operation has a deadline. Let an in-flight read finish
        // and then exit; joining it here would stall UI shutdown for seconds.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_bursts_are_coalesced_and_reports_keep_only_the_latest() {
        let (started, starts) = mpsc::channel();
        let (release, releases) = mpsc::channel();
        let mut count = 0;
        let worker = Worker::spawn(move || {
            count += 1;
            started.send(count).unwrap();
            releases.recv().unwrap();
            Mobile { modem_started: Some(count == 2), ..Mobile::default() }
        }, Duration::from_secs(60)).unwrap();
        assert_eq!(starts.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
        for _ in 0..10_000 { worker.refresh(); }
        release.send(()).unwrap();
        assert_eq!(starts.recv_timeout(Duration::from_secs(1)).unwrap(), 2);
        release.send(()).unwrap();
        assert!(starts.recv_timeout(Duration::from_millis(50)).is_err());
        assert_eq!(worker.taken().unwrap().modem_started, Some(true));
        assert!(worker.taken().is_none());
    }

    #[test]
    fn dropping_during_a_query_returns_immediately_and_stops_before_refresh() {
        struct Finished(Option<mpsc::Sender<()>>);
        impl Drop for Finished {
            fn drop(&mut self) { let _ = self.0.take().unwrap().send(()); }
        }
        let (finished, finish) = mpsc::channel();
        let guard = Finished(Some(finished));
        let (started, starts) = mpsc::channel();
        let (release, releases) = mpsc::channel();
        let worker = Worker::spawn(move || {
            let _ = &guard;
            started.send(()).unwrap();
            releases.recv().unwrap();
            Mobile::default()
        }, Duration::from_secs(60)).unwrap();
        starts.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.refresh();
        drop(worker);
        // This release is possible only if Drop did not wait for the query.
        release.send(()).unwrap();
        finish.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(starts.try_recv(), Err(mpsc::TryRecvError::Disconnected)));
    }
}
