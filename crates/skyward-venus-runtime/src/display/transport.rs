//! Private display owner's fail-closed SOCK_SEQPACKET transport.
//! All generation, sequence, geometry and commit assertions mirror the
//! existing protocol. No Binder or vendor display functions are called here.
use super::protocol::{self, Message};
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
    time::{Duration, Instant},
};

pub(super) struct Owner {
    conn: File,
    last: Message,
    sequence: u64,
}
fn poll(fd: i32, flags: libc::c_short) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        ensure!(!remain.is_zero(), "owner IPC deadline");
        let mut fds = libc::pollfd {
            fd,
            events: flags,
            revents: 0,
        };
        let limit = remain.as_millis().min(i32::MAX as u128) as i32;
        let result = unsafe { libc::poll(&mut fds, 1, limit) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("owner IPC poll");
        }
        ensure!(
            result != 0 && fds.revents & (libc::POLLERR | libc::POLLNVAL) == 0,
            "owner IPC failed or timed out"
        );
        if fds.revents & flags != 0 {
            return Ok(());
        }
        ensure!(fds.revents & libc::POLLHUP == 0, "owner IPC socket closed");
    }
}
fn receive(fd: i32) -> Result<Message> {
    poll(fd, libc::POLLIN)?;
    let mut buffer = [0u8; protocol::LEN + 1];
    let count = unsafe {
        libc::recv(
            fd,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            libc::MSG_TRUNC | libc::MSG_DONTWAIT,
        )
    };
    ensure!(
        count == protocol::LEN as isize,
        "owner IPC packet truncated or changed"
    );
    Message::decode(&buffer[..protocol::LEN]).map_err(anyhow::Error::msg)
}
fn transmit(fd: i32, value: Message) -> Result<()> {
    poll(fd, libc::POLLOUT)?;
    let data = value.encode();
    let sent = unsafe {
        libc::send(
            fd,
            data.as_ptr().cast(),
            data.len(),
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
        )
    };
    ensure!(sent == data.len() as isize, "owner IPC short packet send");
    Ok(())
}
fn valid_response(answer: Message, request: Message, previous: Message) -> Result<()> {
    ensure!(
        answer.kind == protocol::REPLY
            && answer.request == request.request
            && answer.generation == request.generation
            && answer.op == request.op
            && answer.status == 0,
        "owner rejected or mismatched reply"
    );
    ensure!(
        answer.width == previous.width
            && answer.height == previous.height
            && answer.commit >= previous.commit,
        "owner geometry or commit regressed"
    );
    if request.op == protocol::PAUSE {
        ensure!(
            answer.state == protocol::PAUSED,
            "owner did not acknowledge pause"
        );
    }
    if request.op == protocol::RESUME {
        ensure!(
            answer.state == protocol::READY && answer.commit > previous.commit,
            "owner resumed without completed refresh"
        );
    }
    ensure!(
        answer.state == protocol::READY || answer.state == protocol::PAUSED,
        "owner is failed"
    );
    Ok(())
}
impl Owner {
    pub(super) fn connect(path: &Path, expected_pid: libc::pid_t) -> Result<Self> {
        ensure!(
            path.is_absolute() && expected_pid > 0,
            "owner IPC path or PID invalid"
        );
        let bytes = path.as_os_str().as_bytes();
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        ensure!(
            bytes.len() < address.sun_path.len(),
            "owner IPC path too long"
        );
        address.sun_family = libc::AF_UNIX as _;
        for (dst, src) in address.sun_path.iter_mut().zip(bytes) {
            *dst = *src as libc::c_char;
        }
        let fd =
            unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
        ensure!(
            fd >= 0,
            "owner IPC create socket: {}",
            io::Error::last_os_error()
        );
        let conn = unsafe { File::from_raw_fd(fd) };
        let result = unsafe {
            libc::connect(
                fd,
                (&raw const address).cast(),
                size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        ensure!(
            result == 0,
            "owner IPC connect: {}",
            io::Error::last_os_error()
        );
        let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = size_of::<libc::ucred>() as libc::socklen_t;
        let result = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut peer).cast(),
                &mut length,
            )
        };
        ensure!(
            result == 0
                && length as usize == size_of::<libc::ucred>()
                && peer.pid == expected_pid
                && peer.uid == 0,
            "owner process identity changed"
        );
        let hello = receive(fd)?;
        ensure!(
            hello.kind == protocol::HELLO
                && hello.request == 0
                && hello.generation > 0
                && hello.commit >= 1
                && hello.op == 0
                && hello.status == 0
                && hello.width > 0
                && hello.height > 0
                && hello.state == protocol::READY,
            "owner not physically initialized"
        );
        let mut owner = Self {
            conn,
            last: hello,
            sequence: 0,
        };
        owner.call(protocol::PING)?;
        Ok(owner)
    }
    pub(super) fn call(&mut self, operation: u32) -> Result<Message> {
        ensure!(
            matches!(
                operation,
                protocol::PING | protocol::PAUSE | protocol::RESUME
            ),
            "invalid owner operation"
        );
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("owner request counter exhausted")?;
        let request = Message {
            kind: protocol::REQUEST,
            request: self.sequence,
            generation: self.last.generation,
            commit: 0,
            op: operation,
            status: 0,
            width: 0,
            height: 0,
            state: 0,
        };
        transmit(self.conn.as_raw_fd(), request)?;
        let response = receive(self.conn.as_raw_fd())?;
        valid_response(response, request, self.last)?;
        self.last = response;
        Ok(response)
    }
    pub(super) fn socket(&self) -> i32 {
        self.conn.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(op: u32, id: u64) -> Message {
        Message {
            kind: protocol::REQUEST,
            request: id,
            generation: 7,
            commit: 0,
            op,
            status: 0,
            width: 0,
            height: 0,
            state: 0,
        }
    }
    fn prev() -> Message {
        Message {
            kind: protocol::HELLO,
            request: 0,
            generation: 7,
            commit: 2,
            op: 0,
            status: 0,
            width: 1440,
            height: 3200,
            state: protocol::READY,
        }
    }
    #[test]
    fn seqpacket_exact_frames_reject_truncation_and_closed_peer() {
        let mut fds = [-1, -1];
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                    0,
                    fds.as_mut_ptr(),
                )
            },
            0
        );
        let tx = unsafe { File::from_raw_fd(fds[0]) };
        let rx = unsafe { File::from_raw_fd(fds[1]) };
        let valid = request(protocol::RESUME, 2);
        transmit(tx.as_raw_fd(), valid).unwrap();
        assert_eq!(receive(rx.as_raw_fd()).unwrap(), valid);
        let oversize = [0u8; protocol::LEN + 1];
        assert_eq!(
            unsafe {
                libc::send(
                    tx.as_raw_fd(),
                    oversize.as_ptr().cast(),
                    oversize.len(),
                    libc::MSG_NOSIGNAL,
                )
            },
            oversize.len() as isize
        );
        assert!(receive(rx.as_raw_fd()).is_err());
        // Invalid frames must be rejected without accidentally accepting a following packet.
        let mut corrupted = valid.encode();
        corrupted[63] = 1;
        assert_eq!(
            unsafe {
                libc::send(
                    tx.as_raw_fd(),
                    corrupted.as_ptr().cast(),
                    corrupted.len(),
                    libc::MSG_NOSIGNAL,
                )
            },
            corrupted.len() as isize
        );
        assert!(receive(rx.as_raw_fd()).is_err());
        drop(tx);
        assert!(receive(rx.as_raw_fd()).is_err());
    }
    #[test]
    fn exact_owner_response_and_errors() {
        let previous = prev();
        let sent = request(protocol::PAUSE, 1);
        let good = Message {
            kind: protocol::REPLY,
            request: 1,
            generation: 7,
            commit: 2,
            op: protocol::PAUSE,
            status: 0,
            width: 1440,
            height: 3200,
            state: protocol::PAUSED,
        };
        assert!(valid_response(good, sent, previous).is_ok());
        for change in 0..6 {
            let mut bad = good;
            match change {
                0 => bad.request = 2,
                1 => bad.generation = 9,
                2 => bad.width = 0,
                3 => bad.commit = 1,
                4 => bad.state = protocol::FAILED,
                _ => bad.status = -1,
            }
            assert!(valid_response(bad, sent, previous).is_err());
        }
        let resume = request(protocol::RESUME, 2);
        let mut complete = good;
        complete.request = 2;
        complete.op = protocol::RESUME;
        complete.state = protocol::READY;
        assert!(valid_response(complete, resume, previous).is_err());
        complete.commit += 1;
        assert!(valid_response(complete, resume, previous).is_ok());
    }
}
