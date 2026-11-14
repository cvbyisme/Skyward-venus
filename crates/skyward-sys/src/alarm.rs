//! Wake-capable, one-shot display clock alarms. A normal timerfd is never
//! substituted when CAP_WAKE_ALARM or the kernel alarm clock is unavailable.
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlarmEvent {
    Pending,
    Fired(u64),
    /// A discontinuous wall-clock change invalidated the absolute deadline.
    /// Refresh the clock face and arm its next wall-clock minute again.
    ClockChanged,
}

pub struct WakeAlarm {
    fd: OwnedFd,
}

fn next_minute(now_ms: u64) -> io::Result<u64> {
    (now_ms / 60_000).checked_add(1).and_then(|m| m.checked_mul(60_000))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "alarm deadline overflows"))
}

fn realtime_ms() -> io::Result<u64> {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    if unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut now) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if now.tv_sec < 0 || !(0..1_000_000_000).contains(&now.tv_nsec) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid wall clock"));
    }
    (now.tv_sec as u64).checked_mul(1000).and_then(|s| s.checked_add(now.tv_nsec as u64 / 1_000_000))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "wall clock overflows"))
}

impl WakeAlarm {
    /// Requires a genuine kernel wake alarm clock and CAP_WAKE_ALARM.
    pub fn new() -> io::Result<Self> {
        Self::with_clock(libc::CLOCK_REALTIME_ALARM)
    }

    fn with_clock(clock: libc::clockid_t) -> io::Result<Self> {
        let fd = unsafe { libc::timerfd_create(clock, libc::TFD_NONBLOCK | libc::TFD_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Arm exactly one next wall-clock minute. Returns the Unix-millisecond
    /// deadline for logging; callers rearm only after consuming the event.
    pub fn arm_next_minute(&self) -> io::Result<u64> {
        let deadline = next_minute(realtime_ms()?)?;
        self.arm_at(deadline)?;
        Ok(deadline)
    }

    /// Useful for bounded device probes. This still uses the real wake alarm
    /// clock and cancels visibly if wall time is changed while it is armed.
    pub fn arm_after(&self, duration: Duration) -> io::Result<u64> {
        if duration.is_zero() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "an alarm delay must be positive"));
        }
        let delay = u64::try_from(duration.as_millis().max(1))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "alarm delay overflows"))?;
        let deadline = realtime_ms()?.checked_add(delay)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "alarm deadline overflows"))?;
        self.arm_at(deadline)?;
        Ok(deadline)
    }

    pub fn arm_at(&self, unix_ms: u64) -> io::Result<()> {
        if unix_ms == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "zero disarms a timer; use disarm() explicitly"));
        }
        let seconds = libc::time_t::try_from(unix_ms / 1000)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "alarm deadline exceeds time_t"))?;
        let timer = libc::itimerspec {
            it_interval: libc::timespec { tv_sec: 0, tv_nsec: 0 },
            it_value: libc::timespec { tv_sec: seconds, tv_nsec: ((unix_ms % 1000) * 1_000_000) as libc::c_long },
        };
        self.set(&timer, libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET)
    }

    pub fn disarm(&self) -> io::Result<()> {
        let zero = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        self.set(&libc::itimerspec { it_interval: zero, it_value: zero }, 0)
    }

    fn set(&self, timer: &libc::itimerspec, flags: libc::c_int) -> io::Result<()> {
        if unsafe { libc::timerfd_settime(self.fd(), flags, timer, std::ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Consume a ready alarm fd without blocking. Add it to the UI's wakeful
    /// poll/epoll set before allowing suspend; after wake keep suspend denied
    /// until the refreshed AOD frame has settled back into LP2.
    pub fn drain(&self) -> io::Result<AlarmEvent> {
        loop {
            let mut expirations = 0u64;
            let count = unsafe { libc::read(self.fd(), (&mut expirations as *mut u64).cast(), std::mem::size_of::<u64>()) };
            if count < 0 {
                let error = io::Error::last_os_error();
                return match error.raw_os_error() {
                    Some(libc::EINTR) => continue,
                    Some(libc::EAGAIN) => Ok(AlarmEvent::Pending),
                    Some(libc::ECANCELED) => Ok(AlarmEvent::ClockChanged),
                    _ => Err(error),
                };
            }
            if count as usize != std::mem::size_of::<u64>() || expirations == 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "incomplete alarm expiration count"));
            }
            return Ok(AlarmEvent::Fired(expirations));
        }
    }
}

impl AsFd for WakeAlarm {
    fn as_fd(&self) -> BorrowedFd<'_> { self.fd.as_fd() }
}

impl AsRawFd for WakeAlarm {
    fn as_raw_fd(&self) -> RawFd { self.fd() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minute_deadlines_do_not_repeat_the_minute_just_shown() {
        assert_eq!(next_minute(0).unwrap(), 60_000);
        assert_eq!(next_minute(59_999).unwrap(), 60_000);
        assert_eq!(next_minute(60_000).unwrap(), 120_000);
        assert!(next_minute(u64::MAX).is_err());
    }

    #[test]
    fn timerfd_expiration_is_consumed_once_and_disarm_clears_it() {
        // Real Linux timerfd semantics without requiring host CAP_WAKE_ALARM.
        // This test does not assert the host can wake from system suspend.
        let alarm = WakeAlarm::with_clock(libc::CLOCK_REALTIME).unwrap();
        assert_eq!(alarm.drain().unwrap(), AlarmEvent::Pending);
        alarm.arm_after(Duration::from_millis(20)).unwrap();
        let mut fd = libc::pollfd { fd: alarm.fd(), events: libc::POLLIN, revents: 0 };
        assert_eq!(unsafe { libc::poll(&mut fd, 1, 1000) }, 1);
        assert_eq!(alarm.drain().unwrap(), AlarmEvent::Fired(1));
        assert_eq!(alarm.drain().unwrap(), AlarmEvent::Pending);
        alarm.arm_after(Duration::from_millis(20)).unwrap();
        alarm.disarm().unwrap();
        assert_eq!(unsafe { libc::poll(&mut fd, 1, 40) }, 0);
        assert_eq!(alarm.drain().unwrap(), AlarmEvent::Pending);
        assert!(alarm.arm_at(0).is_err());
        assert!(alarm.arm_after(Duration::ZERO).is_err());
    }
}
