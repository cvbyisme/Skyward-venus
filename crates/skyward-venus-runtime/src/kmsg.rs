use crate::common::*;
use anyhow::{Result, bail};
use std::{
    fs::OpenOptions,
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    time::{Duration, Instant},
};
pub fn capture() -> Result<()> {
    recovery()?;
    let mut fd = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open("/dev/kmsg")?;
    let start = Instant::now();
    let mut total = 0;
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "KM41_KMSG_CAPTURE_READY max_bytes=16777216 max_seconds=300"
    )?;
    out.flush()?;
    let mut buffer = [0u8; 65536];
    while total < 16 * 1024 * 1024 && start.elapsed() < Duration::from_secs(300) {
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut poll, 1, 250) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e.into());
        }
        if n == 0 {
            continue;
        }
        match fd.read(&mut buffer) {
            Ok(0) => bail!("unexpected kmsg EOF"),
            Ok(n) => {
                out.write_all(&buffer[..n])?;
                out.flush()?;
                total += n;
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(e) if e.raw_os_error() == Some(libc::EPIPE) => {
                writeln!(out, "KM41_KMSG_OVERRUN: old records lost")?;
                out.flush()?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
