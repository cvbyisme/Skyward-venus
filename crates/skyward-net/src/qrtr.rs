//! Linux QRTR datagrams. Discovery uses the local name server; QMI replies
//! must come from the selected endpoint and match type, transaction and ID.
use crate::qmi::{Response, StatusRequest};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::collections::{BTreeSet, VecDeque};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

const FAMILY: u16 = 42; // AF_QIPCRTR, Linux UAPI.
const CONTROL: u32 = 0xffff_fffe;
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Address { family: u16, node: u32, port: u32 }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Endpoint {
    pub service: u32,
    pub instance: u32,
    pub version: u8,
    pub node: u32,
    pub port: u32,
}

struct Socket(OwnedFd);
impl Socket {
    fn new() -> Result<Self> {
        let fd = unsafe { libc::socket(FAMILY.into(), libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 0) };
        ensure!(fd >= 0, "open QRTR socket: {}", io::Error::last_os_error());
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }
    fn address(&self) -> Result<Address> {
        let mut address = Address::default();
        let mut size = std::mem::size_of::<Address>() as libc::socklen_t;
        ensure!(unsafe { libc::getsockname(self.0.as_raw_fd(), (&mut address as *mut Address).cast(), &mut size) } == 0,
            "read QRTR address: {}", io::Error::last_os_error());
        ensure!(size as usize == std::mem::size_of::<Address>() && address.family == FAMILY, "unexpected QRTR address ABI");
        Ok(address)
    }
    fn send(&self, to: Address, bytes: &[u8]) -> Result<()> {
        let sent = unsafe { libc::sendto(self.0.as_raw_fd(), bytes.as_ptr().cast(), bytes.len(), 0,
            (&to as *const Address).cast(), std::mem::size_of::<Address>() as libc::socklen_t) };
        ensure!(sent >= 0, "send QRTR datagram: {}", io::Error::last_os_error());
        ensure!(sent as usize == bytes.len(), "short QRTR datagram"); Ok(())
    }
    fn receive(&self, until: Instant) -> Result<Option<(Address, Vec<u8>)>> {
        loop {
            let Some(left) = until.checked_duration_since(Instant::now()) else { return Ok(None) };
            let mut poll = libc::pollfd { fd: self.0.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            let ready = unsafe { libc::poll(&mut poll, 1, left.as_millis().clamp(1, i32::MAX as u128) as i32) };
            if ready == 0 { return Ok(None); }
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted { continue; }
                return Err(error).context("wait for QRTR datagram");
            }
            ensure!(poll.revents & libc::POLLIN != 0, "QRTR socket stopped: {}", poll.revents);
            let mut bytes = vec![0; 65542];
            let mut from = Address::default();
            let mut size = std::mem::size_of::<Address>() as libc::socklen_t;
            let n = unsafe { libc::recvfrom(self.0.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len(), libc::MSG_TRUNC,
                (&mut from as *mut Address).cast(), &mut size) };
            if n < 0 {
                let error = io::Error::last_os_error();
                if matches!(error.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) { continue; }
                return Err(error).context("receive QRTR datagram");
            }
            ensure!(n as usize <= bytes.len(), "oversized QRTR datagram");
            ensure!(size as usize == std::mem::size_of::<Address>() && from.family == FAMILY, "foreign QRTR address family");
            bytes.truncate(n as usize); return Ok(Some((from, bytes)));
        }
    }
}

pub fn discover() -> Result<Vec<Endpoint>> {
    let socket = Socket::new()?;
    let local = socket.address()?;
    let mut packet = [0; 20]; packet[..4].copy_from_slice(&10u32.to_le_bytes()); // NEW_LOOKUP, all services.
    socket.send(Address { family: FAMILY, node: local.node, port: CONTROL }, &packet)?;
    let until = Instant::now() + Duration::from_secs(2);
    let mut endpoints = BTreeSet::new();
    while let Some((from, bytes)) = socket.receive(until)? {
        if from.node != local.node || from.port != CONTROL || bytes.len() != 20 { continue; }
        let word = |at| u32::from_le_bytes(bytes[at..at+4].try_into().expect("bounded QRTR control word"));
        if word(0) != 4 { continue; } // NEW_SERVER
        let (node, port) = (word(12),word(16));
        if node == 0 && port == 0 { break; } // End of the current lookup snapshot.
        endpoints.insert(Endpoint { service: word(4), instance: word(8)>>8, version: word(8) as u8, node, port });
        ensure!(endpoints.len() <= 1024, "QRTR discovery exceeded its bound");
    }
    Ok(endpoints.into_iter().collect())
}

/// Each query gets its own QRTR client port and no subscription. Only
/// read-only requests are expressible here; no PIN attempts, SIM power,
/// network attachment, profiles or packet-data state is changed.
pub fn status<T>(endpoint: Endpoint, request: StatusRequest, decode: impl FnOnce(&Response<'_>) -> Result<T>) -> Result<T> {
    ensure!(endpoint.service == request.service(), "QMI status request sent to the wrong service");
    Client::new(endpoint)?.request(request.message(),&[],Duration::from_secs(3),decode)
}

/// Retain one modem client port across stateful transactions. Dropping it
/// releases this client's modem resources, including its packet-data call.
pub(crate) struct Client {
    socket: Socket,
    endpoint: Endpoint,
    transaction: u16,
    indications: IndicationBuffer,
}

/// A bounded diagnostic snapshot for one client, never a state response or
/// permission to adopt/stop a call. Overflow and malformed input are visible.
#[derive(Clone, Debug, Default, Serialize)]
pub struct WdsDiagnostics {
    pub indications: Vec<crate::qmi::PacketServiceIndication>,
    pub discarded: u64,
    pub malformed: u64,
}

#[derive(Default)]
struct IndicationBuffer {
    entries: VecDeque<crate::qmi::PacketServiceIndication>,
    discarded: u64,
    malformed: u64,
}
impl IndicationBuffer {
    fn receive(
        &mut self,
        endpoint: Endpoint,
        from: Address,
        bytes: &[u8],
    ) -> Option<Result<crate::qmi::PacketServiceIndication>> {
        // Only this socket's selected WDS endpoint can supply its events.
        // Other services, peers, messages and response packets stay separate.
        if endpoint.service != 1
            || from.family != FAMILY
            || from.node != endpoint.node
            || from.port != endpoint.port
            || bytes.len() < 5
            || bytes[0] != 4
            || u16::from_le_bytes([bytes[3], bytes[4]]) != 0x22
        {
            return None;
        }
        let decoded = crate::qmi::PacketServiceIndication::decode(bytes);
        match &decoded {
            Ok(status) => {
                if self.entries.len() == 16 {
                    self.entries.pop_front();
                    self.discarded = self.discarded.saturating_add(1);
                }
                self.entries.push_back(*status);
            }
            Err(_) => self.malformed = self.malformed.saturating_add(1),
        }
        Some(decoded)
    }
    fn take(&mut self) -> WdsDiagnostics {
        WdsDiagnostics {
            indications: self.entries.drain(..).collect(),
            discarded: std::mem::take(&mut self.discarded),
            malformed: std::mem::take(&mut self.malformed),
        }
    }
}
impl Client {
    pub(crate) fn new(endpoint: Endpoint) -> Result<Self> {
        Ok(Self {
            socket: Socket::new()?,
            endpoint,
            transaction: 0,
            indications: IndicationBuffer::default(),
        })
    }
    pub(crate) fn take_wds_diagnostics(&mut self) -> WdsDiagnostics {
        self.indications.take()
    }
    pub(crate) fn request<T>(
        &mut self,
        message: u16,
        fields: &[(u8, &[u8])],
        timeout: Duration,
        decode: impl FnOnce(&Response<'_>) -> Result<T>,
    ) -> Result<T> {
        self.transaction = self
            .transaction
            .checked_add(1)
            .context("QMI transaction space exhausted")?;
        let transaction = self.transaction;
        let to = Address {
            family: FAMILY,
            node: self.endpoint.node,
            port: self.endpoint.port,
        };
        self.socket
            .send(to, &crate::qmi::request(transaction, message, fields)?)?;
        let until = Instant::now() + timeout;
        while let Some((from, bytes)) = self.socket.receive(until)? {
            if let Some(indication) = self.indications.receive(self.endpoint, from, &bytes) {
                let client = self.socket.address().ok().map(|address| address.port);
                match indication {
                    Ok(status) => eprintln!(
                        "WDS indication: client={client:?} instance={} node={} peer={} state={} reconfigure={} ip_family={:?}; {}; discarded={}",
                        self.endpoint.instance,
                        self.endpoint.node,
                        self.endpoint.port,
                        status.state,
                        status.reconfiguration_required,
                        status.ip_family,
                        status.reasons,
                        self.indications.discarded
                    ),
                    Err(error) => eprintln!(
                        "WDS indication malformed: client={client:?} instance={} node={} peer={}: {error:#}",
                        self.endpoint.instance, self.endpoint.node, self.endpoint.port
                    ),
                }
                continue;
            }
            if from.node != to.node || from.port != to.port || bytes.len() < 7 || bytes[0] != 2 {
                continue;
            }
            if u16::from_le_bytes([bytes[1], bytes[2]]) != transaction
                || u16::from_le_bytes([bytes[3], bytes[4]]) != message
            {
                continue;
            }
            let response = if self.endpoint.service == 1 && message == 0x20 {
                Response::decode_wds_start(&bytes, transaction)
            } else {
                Response::decode(&bytes, transaction, message)
            }?;
            return decode(&response);
        }
        anyhow::bail!(
            "QMI request {message:#x} timed out for service {} instance {}",
            self.endpoint.service,
            self.endpoint.instance
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn endpoint() -> Endpoint { Endpoint {service:1,instance:0,version:1,node:0,port:42} }
    fn address() -> Address { Address {family:FAMILY,node:0,port:42} }
    fn event(state:u8) -> Vec<u8> { vec![4,0,0,0x22,0,5,0,1,2,0,state,0] }

    #[test]
    fn indications_are_scoped_to_one_selected_wds_peer_and_never_responses() {
        let mut buffer = IndicationBuffer::default();
        let raw = event(1);
        for from in [Address {port:43,..address()},Address {node:1,..address()},Address {family:2,..address()}] {
            assert!(buffer.receive(endpoint(),from,&raw).is_none());
        }
        assert!(buffer.receive(Endpoint {service:3,..endpoint()},address(),&raw).is_none());
        let mut response = raw.clone();response[0]=2;
        assert!(buffer.receive(endpoint(),address(),&response).is_none());
        let mut other = raw.clone();other[3]=0x23;
        assert!(buffer.receive(endpoint(),address(),&other).is_none());
        assert!(buffer.take().indications.is_empty());
        assert_eq!(buffer.receive(endpoint(),address(),&raw).unwrap().unwrap().state,1);
        assert_eq!(buffer.take().indications.len(),1);
    }

    #[test]
    fn indication_storage_is_bounded_and_reports_lost_or_malformed_records() {
        let mut buffer=IndicationBuffer::default();
        for state in 0..20 { buffer.receive(endpoint(),address(),&event(state)).unwrap().unwrap(); }
        assert!(buffer.receive(endpoint(),address(),&event(1)[..10]).unwrap().is_err());
        let report=buffer.take();
        assert_eq!((report.indications.len(),report.discarded,report.malformed),(16,4,1));
        assert_eq!((report.indications[0].state,report.indications[15].state),(4,19));
        let drained=buffer.take();
        assert!(drained.indications.is_empty());assert_eq!((drained.discarded,drained.malformed),(0,0));
    }
}
