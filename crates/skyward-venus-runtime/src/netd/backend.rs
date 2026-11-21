//! Linux rtnetlink UAPI (fib_rules.h/rtnetlink.h) and exact argv iptables calls.
use super::manager::{Backend, Kind, Operation, Result};
use std::{
    collections::BTreeMap,
    ffi::CString,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
const NEW_ROUTE: u16 = 24;
const GET_ROUTE: u16 = 26;
const NEW_RULE: u16 = 32;
const GET_RULE: u16 = 34;
const REQUEST: u16 = 1;
const ACK: u16 = 4;
const DUMP: u16 = 0x300;
const CREATE_EXCL: u16 = 0x600;
const TABLE: u16 = 15; // FRA_TABLE and RTA_TABLE
const PRIORITY: u16 = 6;
const IIF: u16 = 3;
const OIF: u16 = 17;
const FWMARK: u16 = 10;
const FWMASK: u16 = 16;
const UID: u16 = 20;
type Attributes = BTreeMap<u16, Vec<u8>>;
fn errno() -> i32 {
    -std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}
fn u16_at(bytes: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_ne_bytes(
        bytes
            .get(at..at + 2)
            .ok_or(-libc::EBADMSG)?
            .try_into()
            .unwrap(),
    ))
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_ne_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(-libc::EBADMSG)?
            .try_into()
            .unwrap(),
    ))
}
fn aligned(size: usize) -> usize {
    (size + 3) & !3
}
fn message(kind: u16, flags: u16, payload: &[u8]) -> Vec<u8> {
    let mut data = vec![0; 16];
    data[4..6].copy_from_slice(&kind.to_ne_bytes());
    data[6..8].copy_from_slice(&flags.to_ne_bytes());
    data[8..12].copy_from_slice(&1u32.to_ne_bytes());
    data.extend_from_slice(payload);
    data.resize(aligned(data.len()), 0);
    let len = data.len() as u32;
    data[..4].copy_from_slice(&len.to_ne_bytes());
    data
}
fn attr(data: &mut Vec<u8>, kind: u16, value: &[u8]) {
    data.extend_from_slice(&((value.len() + 4) as u16).to_ne_bytes());
    data.extend_from_slice(&kind.to_ne_bytes());
    data.extend_from_slice(value);
    data.resize(aligned(data.len()), 0);
    let len = data.len() as u32;
    data[..4].copy_from_slice(&len.to_ne_bytes());
}
fn string(data: &mut Vec<u8>, kind: u16, value: &str) {
    let mut raw = value.as_bytes().to_vec();
    raw.push(0);
    attr(data, kind, &raw);
}
pub fn request(op: &Operation, add: bool) -> Vec<u8> {
    let flags = REQUEST | ACK | if add { CREATE_EXCL } else { 0 };
    let mut header = [0; 12];
    header[0] = op.family;
    if op.kind == Kind::Rule {
        header[7] = 1; // FR_ACT_TO_TBL
        let mut msg = message(if add { NEW_RULE } else { NEW_RULE + 1 }, flags, &header);
        attr(&mut msg, TABLE, &op.table.to_ne_bytes());
        attr(&mut msg, PRIORITY, &op.priority.to_ne_bytes());
        string(&mut msg, IIF, "lo");
        if !op.oif.is_empty() {
            string(&mut msg, OIF, &op.oif);
        }
        if op.mask != 0 {
            attr(&mut msg, FWMARK, &op.value.to_ne_bytes());
            attr(&mut msg, FWMASK, &op.mask.to_ne_bytes());
        }
        if op.root_only {
            attr(&mut msg, UID, &[0; 8]);
        }
        msg
    } else {
        header[1] = op.prefix;
        header[5] = 4;
        header[6] = if op.gateway { 0 } else { 253 };
        header[7] = 1; // STATIC, UNIVERSE/LINK, UNICAST
        let mut msg = message(if add { NEW_ROUTE } else { NEW_ROUTE + 1 }, flags, &header);
        let size = if op.family == libc::AF_INET as u8 {
            4
        } else {
            16
        };
        attr(&mut msg, TABLE, &op.table.to_ne_bytes());
        attr(&mut msg, 1, &op.destination[..size]);
        attr(&mut msg, 4, &op.index.to_ne_bytes());
        if op.gateway {
            attr(&mut msg, 5, &op.next[..size]);
        }
        msg
    }
}
fn attributes(message: &[u8]) -> Result<Attributes> {
    let mut bytes = message.get(28..).ok_or(-libc::EBADMSG)?;
    let mut result = Attributes::new();
    while !bytes.is_empty() {
        let length = u16_at(bytes, 0)? as usize;
        let kind = u16_at(bytes, 2)?;
        if length < 4 || aligned(length) > bytes.len() {
            return Err(-libc::EBADMSG);
        }
        if result.insert(kind, bytes[4..length].to_vec()).is_some() {
            return Err(-libc::EBADMSG);
        }
        bytes = &bytes[aligned(length)..];
    }
    Ok(result)
}
fn eq(a: &Attributes, kind: u16, value: &[u8]) -> bool {
    a.get(&kind).is_some_and(|v| v == value)
}
fn eq_string(a: &Attributes, kind: u16, value: &str) -> bool {
    a.get(&kind)
        .is_some_and(|v| v.strip_suffix(&[0]) == Some(value.as_bytes()))
}
fn table(message: &[u8], a: &Attributes) -> Result<u32> {
    match a.get(&TABLE) {
        Some(value) => {
            if value.len() != 4 {
                return Err(-libc::EBADMSG);
            }
            u32_at(value, 0)
        }
        None => Ok(*message.get(20).ok_or(-libc::EBADMSG)? as u32),
    }
}
fn matches(message: &[u8], a: &Attributes, op: &Operation) -> Result<bool> {
    if table(message, a)? != op.table {
        return Ok(false);
    }
    let r = message.get(16..28).ok_or(-libc::EBADMSG)?;
    if op.kind == Kind::Rule {
        if r[0] != op.family
            || r[7] != 1
            || r[1] != 0
            || r[2] != 0
            || r[3] != 0
            || u32_at(r, 8)? & 2 != 0
        {
            return Ok(false);
        }
        if !eq(a, PRIORITY, &op.priority.to_ne_bytes()) || !eq_string(a, IIF, "lo") {
            return Ok(false);
        }
        if if op.mask != 0 {
            !eq(a, FWMARK, &op.value.to_ne_bytes()) || !eq(a, FWMASK, &op.mask.to_ne_bytes())
        } else {
            a.contains_key(&FWMARK) || a.contains_key(&FWMASK)
        } {
            return Ok(false);
        }
        if if op.oif.is_empty() {
            a.contains_key(&OIF)
        } else {
            !eq_string(a, OIF, &op.oif)
        } {
            return Ok(false);
        }
        if if op.root_only {
            !eq(a, UID, &[0; 8])
        } else {
            a.contains_key(&UID)
        } {
            return Ok(false);
        }
        for kind in a.keys() {
            match *kind {
                TABLE | PRIORITY | IIF | OIF | FWMARK | FWMASK | UID | 21 => {}
                13 | 14 if eq(a, *kind, &u32::MAX.to_ne_bytes()) => {}
                _ => return Ok(false), // Never delete a foreign selector.
            }
        }
    } else {
        let size = if op.family == libc::AF_INET as u8 {
            4
        } else {
            16
        };
        let scope = if op.gateway || size == 16 { 0 } else { 253 };
        if r[0] != op.family
            || r[1] != op.prefix
            || r[2] != 0
            || r[3] != 0
            || r[5] != 4
            || r[7] != 1
            || r[6] != scope
            || !eq(a, 4, &op.index.to_ne_bytes())
        {
            return Ok(false);
        }
        if match a.get(&1) {
            Some(v) => v != &op.destination[..size],
            None => op.prefix != 0,
        } {
            return Ok(false);
        }
        if if op.gateway {
            !eq(a, 5, &op.next[..size])
        } else {
            a.contains_key(&5)
        } {
            return Ok(false);
        }
        for kind in a.keys() {
            match *kind {
                TABLE | 1 | 5 | 4 | 12 => {}
                6 if size == 16 && eq(a, 6, &1024u32.to_ne_bytes()) => {}
                20 if eq(a, 20, &[0]) => {}
                _ => return Ok(false),
            }
        }
    }
    Ok(true)
}
// Returns true only after an ACK or complete dump. Incomplete/truncated dumps
// must never turn into proof that a routing table or object is absent.
fn consume(
    mut bytes: &[u8],
    dump: bool,
    visit: &mut impl FnMut(&[u8]) -> Result<()>,
) -> Result<bool> {
    while !bytes.is_empty() {
        let length = u32_at(bytes, 0)? as usize;
        if length < 16
            || length > bytes.len()
            || aligned(length) > bytes.len()
            || u32_at(bytes, 8)? != 1
        {
            return Err(-libc::EBADMSG);
        }
        let message = &bytes[..length];
        if u16_at(message, 6)? & 16 != 0 {
            return Err(-libc::EINTR);
        }
        match u16_at(message, 4)? {
            2 => {
                // NLMSG_ERROR includes ACK with error=0
                if length < 36 {
                    return Err(-libc::EBADMSG);
                }
                let error = u32_at(message, 16)? as i32;
                if error != 0 {
                    return Err(if error < 0 { error } else { -libc::EBADMSG });
                }
                if !dump {
                    return Ok(true);
                }
            }
            3 => {
                if !dump || (length > 16 && length < 20) {
                    return Err(-libc::EBADMSG);
                }
                if length >= 20 {
                    let error = u32_at(message, 16)? as i32;
                    if error != 0 {
                        return Err(if error < 0 { error } else { -libc::EBADMSG });
                    }
                }
                return Ok(true);
            }
            _ => visit(message)?,
        }
        bytes = &bytes[aligned(length)..];
    }
    Ok(false)
}
fn exchange(request: &[u8], dump: bool, mut visit: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_ROUTE,
        )
    };
    if fd < 0 {
        return Err(errno());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut local: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    local.nl_family = libc::AF_NETLINK as u16;
    let length = size_of::<libc::sockaddr_nl>() as libc::socklen_t;
    if unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&local as *const libc::sockaddr_nl).cast(),
            length,
        )
    } != 0
    {
        return Err(errno());
    }
    let sent = unsafe {
        libc::sendto(
            fd.as_raw_fd(),
            request.as_ptr().cast(),
            request.len(),
            0,
            (&local as *const libc::sockaddr_nl).cast(),
            length,
        )
    };
    if sent < 0 {
        return Err(errno());
    }
    if sent as usize != request.len() {
        return Err(-libc::EIO);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        if remaining == 0 {
            return Err(-libc::ETIMEDOUT);
        }
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll, 1, remaining as i32) };
        if ready < 0 {
            if errno() == -libc::EINTR {
                continue;
            }
            return Err(errno());
        }
        if ready == 0 {
            return Err(-libc::ETIMEDOUT);
        }
        if poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(-libc::EIO);
        }
        let mut buffer = [0u8; 65536];
        let mut sender: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        let mut io = libc::iovec {
            iov_base: buffer.as_mut_ptr().cast(),
            iov_len: buffer.len(),
        };
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_name = (&mut sender as *mut libc::sockaddr_nl).cast();
        msg.msg_namelen = length;
        msg.msg_iov = &mut io;
        msg.msg_iovlen = 1;
        let bytes = unsafe { libc::recvmsg(fd.as_raw_fd(), &mut msg, 0) };
        if bytes < 0 {
            let error = errno();
            if error == -libc::EAGAIN || error == -libc::EINTR {
                continue;
            }
            return Err(error);
        }
        if bytes == 0
            || msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
            || msg.msg_namelen != length
            || sender.nl_family != libc::AF_NETLINK as u16
            || sender.nl_pid != 0
        {
            return Err(-libc::EBADMSG);
        }
        if consume(&buffer[..bytes as usize], dump, &mut visit)? {
            return Ok(());
        }
    }
}
fn dump(
    family: u8,
    rules: bool,
    mut visitor: impl FnMut(&[u8], &Attributes) -> Result<()>,
) -> Result<()> {
    let mut payload = [0; 12];
    payload[0] = family;
    exchange(
        &message(
            if rules { GET_RULE } else { GET_ROUTE },
            REQUEST | DUMP,
            &payload,
        ),
        true,
        |msg| {
            if u16_at(msg, 4)? != if rules { NEW_RULE } else { NEW_ROUTE } {
                return Err(-libc::EBADMSG);
            }
            visitor(msg, &attributes(msg)?)
        },
    )
}
pub struct LinuxBackend {
    pub tool_dir: PathBuf,
}
impl Default for LinuxBackend {
    fn default() -> Self {
        Self {
            tool_dir: "/system/bin".into(),
        }
    }
}
impl LinuxBackend {
    fn iptables(&self, op: &Operation, action: &str) -> Result<i32> {
        let tool = self.tool_dir.join(if op.family == libc::AF_INET as u8 {
            "iptables"
        } else {
            "ip6tables"
        });
        let mut child = Command::new(tool)
            .args([
                "-w",
                "1",
                "-t",
                "mangle",
                action,
                "INPUT",
                "-i",
                &op.iface,
                "-m",
                "comment",
                "--comment",
                &op.tag,
                "-j",
                "MARK",
                "--set-mark",
                &format!("0x{:x}/0x{:x}", op.value, op.mask),
            ])
            .spawn()
            .map_err(|e| -e.raw_os_error().unwrap_or(libc::EIO))?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.code().ok_or(-libc::EIO),
                Ok(None) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(-e.raw_os_error().unwrap_or(libc::EIO));
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(-libc::ETIMEDOUT);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn change(&self, op: &Operation, add: bool) -> Result<()> {
        if op.kind == Kind::Mark {
            if self.iptables(op, if add { "-A" } else { "-D" })? == 0 {
                Ok(())
            } else {
                Err(-libc::EIO)
            }
        } else {
            exchange(&request(op, add), false, |_| Err(-libc::EBADMSG))
        }
    }
}
impl Backend for LinuxBackend {
    fn index(&mut self, name: &str) -> u32 {
        CString::new(name).map_or(0, |name| unsafe { libc::if_nametoindex(name.as_ptr()) })
    }
    fn unused_table(&mut self, target: u32) -> Result<()> {
        let mut used = false;
        for family in [libc::AF_INET as u8, libc::AF_INET6 as u8] {
            for rules in [false, true] {
                dump(family, rules, |m, a| {
                    if table(m, a)? == target {
                        used = true;
                    }
                    Ok(())
                })?;
            }
        }
        if used { Err(-libc::EEXIST) } else { Ok(()) }
    }
    fn exists(&mut self, op: &Operation) -> Result<bool> {
        if op.kind == Kind::Mark {
            return match self.iptables(op, "-C")? {
                0 => Ok(true),
                1 => Ok(false),
                _ => Err(-libc::EIO),
            };
        }
        let mut count = 0;
        dump(op.family, op.kind == Kind::Rule, |m, a| {
            if matches(m, a, op)? {
                count += 1;
            }
            Ok(())
        })?;
        match count {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(-libc::EUCLEAN),
        }
    }
    fn add(&mut self, op: &Operation) -> Result<()> {
        self.change(op, true)
    }
    fn remove(&mut self, op: &Operation) -> Result<()> {
        self.change(op, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rule_wire_and_foreign_selectors_are_exact() {
        let op =
            super::super::manager::interface_operations(1, "rmnet_data0", 49, "owner")[2].clone();
        let mut msg = request(&op, true);
        let attrs = attributes(&msg).unwrap();
        assert_eq!(u16_at(&msg, 4).unwrap(), NEW_RULE);
        assert_ne!(u16_at(&msg, 6).unwrap() & 0x200, 0);
        assert!(eq(&attrs, UID, &[0; 8]));
        assert!(eq(&attrs, TABLE, &1049u32.to_ne_bytes()));
        assert!(eq_string(&attrs, IIF, "lo"));
        assert!(matches(&msg, &attrs, &op).unwrap());
        attr(&mut msg, 22, &[6]);
        assert!(!matches(&msg, &attributes(&msg).unwrap(), &op).unwrap());
    }
    #[test]
    fn truncated_interrupted_and_duplicate_attributes_are_not_absence() {
        let done = message(3, 0, &0i32.to_ne_bytes());
        assert!(consume(&done, true, &mut |_| Ok(())).unwrap());
        for n in 1..done.len() {
            assert!(consume(&done[..n], true, &mut |_| Ok(())).is_err());
        }
        let interrupted = message(3, 16, &0i32.to_ne_bytes());
        assert_eq!(
            consume(&interrupted, true, &mut |_| Ok(())),
            Err(-libc::EINTR)
        );
        let mut op = request(&Operation::default(), true);
        attr(&mut op, TABLE, &0u32.to_ne_bytes());
        assert!(attributes(&op).is_err());
        let mut spoofed = done;
        spoofed[8..12].copy_from_slice(&2u32.to_ne_bytes());
        assert!(consume(&spoofed, true, &mut |_| Ok(())).is_err());
    }
    #[test]
    fn kernel_dumps_are_read_only_and_complete() {
        let value = LinuxBackend::default().unused_table(0xffffffe0);
        assert!(
            matches!(value, Ok(()) | Err(-17)),
            "read-only kernel dump: {value:?}"
        );
    }
}

/// Configuration-only default-route snapshot for the diagnostic client.
#[cfg(target_os = "android")]
pub(super) fn default_routes() -> Result<std::collections::BTreeSet<String>> {
    let mut rows = std::collections::BTreeSet::new();
    for family in [libc::AF_INET as u8, libc::AF_INET6 as u8] {
        dump(family, false, |msg, attributes| {
            if msg[16] != family || msg[17] != 0 {
                return Ok(());
            }
            let mut row = msg[16..28]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            for (kind, value) in attributes {
                if [12, 23].contains(kind) {
                    continue;
                } // CACHEINFO and EXPIRES are not configuration.
                row.push_str(&format!(" {kind}="));
                for byte in value {
                    row.push_str(&format!("{byte:02x}"));
                }
            }
            rows.insert(row);
            Ok(())
        })?;
    }
    Ok(rows)
}
