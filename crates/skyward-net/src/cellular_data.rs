//! One explicitly owned IPv4 WDS call over the native Venus data stack.
//! The caller owns service, interface, route and DNS lifecycles. This module
//! never creates a DPM port, changes WDA, writes a profile or supplies an APN.
#[cfg(any(target_os = "android", test))]
use crate::qmi::{DataFormat, Response};
#[cfg(any(target_os = "android", test))]
use anyhow::{ensure, Context, Result};
use std::net::Ipv4Addr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub address: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub gateway: Ipv4Addr,
    pub dns: Vec<Ipv4Addr>,
    pub mtu: u32,
}

/// WDS wire states are not a boolean. In particular, a suspended or
/// authenticating session still belongs to its client and must not be stopped
/// merely because it cannot currently carry traffic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum PacketState { Disconnected = 1, Connected = 2, Suspended = 3, Authenticating = 4 }

#[cfg(any(target_os = "android", test))]
fn packet_state(response: &Response<'_>) -> Result<PacketState> {
    match response.required(1)? {
        [1] => Ok(PacketState::Disconnected),
        [2] => Ok(PacketState::Connected),
        [3] => Ok(PacketState::Suspended),
        [4] => Ok(PacketState::Authenticating),
        [value] => anyhow::bail!("unknown WDS packet-service state {value}"),
        value => anyhow::bail!("invalid WDS packet-service status length {}", value.len()),
    }
}

#[cfg(any(target_os = "android", test))]
fn wait_connected_with(
    mut read: impl FnMut(std::time::Duration) -> Result<PacketState>,
    remaining: impl Fn() -> std::time::Duration,
    mut sleep: impl FnMut(std::time::Duration),
    mut observed: impl FnMut(PacketState),
) -> Result<()> {
    use std::time::Duration;
    let mut last = None;
    loop {
        let left = remaining();
        ensure!(!left.is_zero(), "WDS setup deadline expired; last state {last:?}; ownership retained");
        let state = read(left.min(Duration::from_secs(3)))?;
        observed(state);
        last = Some(state);
        match state {
            PacketState::Connected => return Ok(()),
            PacketState::Disconnected => anyhow::bail!("WDS explicitly disconnected during setup; ownership retained for verified cleanup"),
            PacketState::Suspended | PacketState::Authenticating => {},
        }
        sleep(remaining().min(Duration::from_millis(200)));
    }
}

#[cfg(any(target_os = "android", test))]
fn managed_format(format: &DataFormat) -> Result<()> {
    ensure!(format.link_layer == 2 && format.uplink_aggregation == Some(9)
        && format.downlink_aggregation == Some(9),
        "native data stack has not negotiated the verified raw-IP/QMAPv5 format");
    Ok(())
}

#[cfg(any(target_os = "android", test))]
fn unicast(address: Ipv4Addr) -> bool {
    let first = address.octets()[0];
    first != 0 && first < 224 && !address.is_loopback() && !address.is_broadcast()
}

#[cfg(any(target_os = "android", test))]
fn settings(response: &Response<'_>) -> Result<Settings> {
    let dword = |tag| -> Result<u32> {
        Ok(u32::from_le_bytes(response.required(tag)?.try_into()
            .context("invalid WDS IPv4 settings field length")?))
    };
    let address = Ipv4Addr::from(dword(0x1e)?);
    let netmask = Ipv4Addr::from(dword(0x21)?);
    let gateway = Ipv4Addr::from(dword(0x20)?);
    let mask = u32::from(netmask);
    let inverse = !mask;
    ensure!(mask != 0 && inverse & inverse.wrapping_add(1) == 0,
        "WDS netmask is not a supported contiguous IPv4 mask");
    ensure!(unicast(address) && unicast(gateway), "WDS returned an unusable IPv4 address or gateway");
    ensure!(u32::from(address) & mask == u32::from(gateway) & mask,
        "WDS gateway is outside the assigned IPv4 subnet");
    if inverse > 1 {
        ensure!(u32::from(address) & inverse != 0 && u32::from(address) & inverse != inverse,
            "WDS assigned the subnet or broadcast address");
    }
    let mut dns = Vec::new();
    for tag in [0x15, 0x16] {
        if response.optional(tag).is_none() { continue; }
        let server = Ipv4Addr::from(dword(tag)?);
        // A missing/zero secondary DNS is allowed; an invalid nonzero one
        // is not silently published into the shared resolver.
        if server.is_unspecified() { continue; }
        ensure!(unicast(server), "WDS returned an unusable IPv4 DNS server");
        if !dns.contains(&server) { dns.push(server); }
    }
    ensure!(!dns.is_empty(), "WDS returned no usable IPv4 DNS servers");
    let mtu = dword(0x29)?;
    ensure!((576..=65_535).contains(&mtu), "WDS MTU is outside the supported IPv4 range");
    Ok(Settings { address, netmask, gateway, dns, mtu })
}

// A Stop acknowledgement, including NO_EFFECT, cannot establish teardown.
// An independent client on the same subscription/mux must agree with the
// owning client. Unknown/error/contradictory evidence preserves ownership.
#[cfg(any(target_os = "android", test))]
fn stopped(owner: Option<PacketState>, independent: Option<PacketState>) -> bool {
    owner == Some(PacketState::Disconnected) && independent == Some(PacketState::Disconnected)
}

// Record an ambiguous transaction before sending it. A timeout, malformed
// handle or rejected reply must leave the client available for confirmation.
#[cfg(any(target_os = "android", test))]
fn begin(handle: &mut Option<u32>, start_attempted: &mut bool, send: impl FnOnce() -> Result<u32>) -> Result<()> {
    ensure!(handle.is_none() && !*start_attempted, "previous WDS start has not been cleared");
    *start_attempted = true;
    let received = send()?;
    ensure!(received != 0 && received != u32::MAX, "WDS returned an invalid or wildcard handle");
    *handle = Some(received);
    Ok(())
}

#[cfg(any(target_os = "android", test))]
fn require_modem_offline(devices: &std::path::Path) -> Result<()> {
    let mut found = 0;
    for entry in std::fs::read_dir(devices)? {
        let path = entry?.path();
        if std::fs::read_to_string(path.join("name"))?.trim() != "modem" { continue; }
        found += 1;
        ensure!(std::fs::read_to_string(path.join("state"))?.trim() == "OFFLINE",
            "kernel modem is not confirmed OFFLINE; WDS ownership retained");
    }
    ensure!(found == 1, "expected one kernel modem state; WDS ownership retained");
    Ok(())
}

#[cfg(any(target_os = "android", test))]
fn release_after_offline(handle: &mut Option<u32>, start_attempted: &mut bool, devices: &std::path::Path) -> Result<()> {
    require_modem_offline(devices)?;
    *handle = None;
    *start_attempted = false;
    Ok(())
}

#[cfg(target_os = "android")]
mod runtime {
    use super::*;
    use crate::{qmi, qrtr};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    static OWNED: AtomicBool = AtomicBool::new(false);
    const QUERY: Duration = Duration::from_secs(3);

    struct Exclusive;
    impl Exclusive {
        fn take() -> Result<Self> {
            ensure!(OWNED.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok(),
                "this process already owns a cellular data operation");
            Ok(Self)
        }
    }
    impl Drop for Exclusive { fn drop(&mut self) { OWNED.store(false, Ordering::Release); } }

    fn primary(services: &[qrtr::Endpoint], service: u32) -> Result<qrtr::Endpoint> {
        let found: Vec<_> = services.iter().copied()
            .filter(|e| e.service == service && e.instance == 0 && e.node == 0).collect();
        ensure!(found.len() == 1, "expected one primary QMI service {service}");
        Ok(found[0])
    }

    fn remaining(until: Instant, limit: Duration) -> Result<Duration> {
        let left = until.saturating_duration_since(Instant::now());
        ensure!(!left.is_zero(), "cellular operation deadline expired");
        Ok(left.min(limit))
    }

    fn ipa_endpoint() -> Result<[u8; 8]> {
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        ensure!(fd >= 0, "open IPA query socket: {}", std::io::Error::last_os_error());
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
        for (out, byte) in request.ifr_name.iter_mut().zip(b"rmnet_ipa0") { *out = *byte as libc::c_char; }
        // The downstream Venus rmnet extended-ioctl union is 32 bytes.
        let mut data = [0u32; 8]; data[0] = 3;
        request.ifr_ifru.ifru_data = data.as_mut_ptr().cast();
        ensure!(unsafe { libc::ioctl(fd.as_raw_fd(), 0x89fd, &mut request) } == 0,
            "read IPA endpoint: {}", std::io::Error::last_os_error());
        let mut endpoint = [0; 8];
        endpoint[..4].copy_from_slice(&4u32.to_le_bytes());
        endpoint[4..].copy_from_slice(&data[1].to_le_bytes());
        Ok(endpoint)
    }

    fn bound(endpoint: qrtr::Endpoint, ipa: &[u8; 8], until: Instant) -> Result<qrtr::Client> {
        let mut client = qrtr::Client::new(endpoint)?;
        client.request(0xaf, &[(1, &1u32.to_le_bytes())], remaining(until, QUERY)?, |_| Ok(()))?;
        client.request(0xa2, &[(0x10, ipa), (0x11, &[1]), (0x13, &1u32.to_le_bytes())],
            remaining(until, QUERY)?, |_| Ok(()))?;
        client.request(0x4d, &[(1, &[4])], remaining(until, QUERY)?, |_| Ok(()))?;
        Ok(client)
    }

    /// The current primary subscription's owned IPv4 call. Physical SIM-slot
    /// selection is deliberately not claimed: UIM provisioning mapping is a
    /// separate operation. Keep this object in the independent radio owner.
    /// Call `stop` and require success before releasing routes or services.
    #[must_use = "retain the WDS session and explicitly verify stop before teardown"]
    pub struct Session {
        client: qrtr::Client,
        endpoint: qrtr::Endpoint,
        ipa: [u8; 8],
        handle: Option<u32>,
        start_attempted: bool,
        profile: u8,
        _exclusive: Exclusive,
    }

    impl Session {
        /// Prepare the modem's current default 3GPP profile. A native
        /// rmnet_data0 and QMAPv5 must already exist. No APN/NV is changed.
        /// Store this session before calling `start`, so a timed-out Start
        /// transaction cannot lose the client that may own its result.
        /// The caller must also exclude external owners of this mux.
        pub fn prepare() -> Result<Self> {
            let exclusive = Exclusive::take()?;
            ensure!(unsafe { libc::geteuid() } == 0 && std::path::Path::new("/skyward/venus-recovery.txt").is_file(),
                "cellular data requires marked Venus Recovery and root");
            let until = Instant::now() + Duration::from_secs(75);
            let link = std::fs::read_to_string("/sys/class/net/rmnet_data0/iflink")
                .context("native netmgr has not created rmnet_data0")?;
            let parent = std::fs::read_to_string("/sys/class/net/rmnet_ipa0/ifindex")?;
            ensure!(link.trim() == parent.trim(), "native data channel does not belong to the IPA parent");
            let services = qrtr::discover()?;
            let ipa = ipa_endpoint()?;
            let mut wda = qrtr::Client::new(primary(&services, 26)?)?;
            managed_format(&wda.request(0x21, &[(0x10, &ipa)], remaining(until, QUERY)?, qmi::data_format)?)?;
            let network = qrtr::status(primary(&services, 3)?, qmi::StatusRequest::ServingSystem, qmi::serving_system)?;
            ensure!(network.registration_state == 1 && network.packet_attach_state == 1,
                "primary modem is not registered for packet data");
            let endpoint = primary(&services, 1)?;
            let mut client = bound(endpoint, &ipa, until)?;
            ensure!(client.request(0x22, &[], remaining(until, QUERY)?, packet_state)? == PacketState::Disconnected,
                "primary data channel is already active; refusing to adopt or stop another call");
            let profile = client.request(0x49, &[(1, &[0, 0])], remaining(until, QUERY)?, |r| {
                let field = r.required(1)?;
                ensure!(field.len() == 1, "invalid default 3GPP profile index length");
                Ok(field[0])
            })?;
            Ok(Self { client, endpoint, ipa, handle: None, start_attempted: false, profile, _exclusive: exclusive })
        }

        /// Start the prepared call. Even on error, retain this session until
        /// `stop` confirms disconnect or the kernel confirms radio OFFLINE.
        pub fn start(&mut self) -> Result<()> {
            let client = &mut self.client;
            let profile = self.profile;
            begin(&mut self.handle, &mut self.start_attempted, || {
                client.request(0x20, &[(0x19, &[4]), (0x31, &[profile])], Duration::from_secs(45), |r| {
                    Ok(u32::from_le_bytes(r.required(1)?.try_into().context("invalid WDS call handle length")?))
                }).context("start primary IPv4 call; a timeout is not a confirmed disconnect")
            })
        }

        pub fn profile(&self) -> u8 { self.profile }

        /// Drain numeric events received by this exact owning WDS client.
        /// They are diagnostics, not a replacement for independently verified
        /// setup/teardown. Reading them never sends a modem command.
        pub fn take_diagnostics(&mut self) -> qrtr::WdsDiagnostics { self.client.take_wds_diagnostics() }

        fn state_with_timeout(&mut self, timeout: Duration) -> Result<PacketState> {
            if self.handle.is_none() {
                ensure!(!self.start_attempted, "WDS start outcome is unknown; ownership retained");
                return Ok(PacketState::Disconnected);
            }
            self.client.request(0x22, &[], timeout, packet_state)
        }

        pub fn state(&mut self) -> Result<PacketState> { self.state_with_timeout(QUERY) }

        /// Observe asynchronous setup without sending another Start or Stop.
        /// Only the real Connected state establishes readiness.
        pub fn wait_connected(&mut self, timeout: Duration, observed: impl FnMut(PacketState)) -> Result<()> {
            let until = Instant::now() + timeout;
            wait_connected_with(|timeout| self.state_with_timeout(timeout),
                || until.saturating_duration_since(Instant::now()), std::thread::sleep, observed)
        }

        /// Read only the network fields used by the interface owner. No APN,
        /// profile names, subscriber IDs, username or password are requested.
        pub fn settings(&mut self) -> Result<Settings> {
            let state = self.state()?;
            ensure!(state == PacketState::Connected, "owned WDS call is {state:?}; ownership retained");
            self.client.request(0x2d, &[(0x10, &0x2310u32.to_le_bytes())], QUERY, super::settings)
        }

        /// Stop only our returned handle, then confirm that both our client
        /// and an independently bound client report disconnected. A failed
        /// stop preserves the handle and returns an error; callers must not
        /// mark teardown complete or silently begin another call.
        pub fn stop(&mut self) -> Result<()> {
            if self.handle.is_none() && !self.start_attempted { return Ok(()); }
            let until = Instant::now() + Duration::from_secs(20);
            // An unknown Start has no safe handle to stop. Never invent a
            // wildcard: only independently confirmed disconnect releases it.
            let acknowledgement = if let Some(handle) = self.handle {
                Some(self.client.request(0x21, &[(1, &handle.to_le_bytes())],
                    remaining(until, Duration::from_secs(5))?, |_| Ok(())))
            } else { None };
            let mut independent = bound(self.endpoint, &self.ipa, until)
                .context("cannot independently confirm WDS disconnect; ownership retained")?;
            loop {
                let owner = self.client.request(0x22, &[], remaining(until, QUERY)?, packet_state).ok();
                let other = independent.request(0x22, &[], remaining(until, QUERY)?, packet_state).ok();
                if stopped(owner, other) {
                    self.handle = None;
                    self.start_attempted = false;
                    return Ok(());
                }
                if until.saturating_duration_since(Instant::now()) <= Duration::from_millis(200) {
                    let detail = acknowledgement.and_then(Result::err).map(|e| format!("; stop: {e:#}")).unwrap_or_default();
                    anyhow::bail!("WDS disconnect was not confirmed; ownership retained{detail}");
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }

        /// A complete radio shutdown is stronger evidence than a WDS reply.
        /// Independently verify the kernel state before releasing an unknown
        /// or known call after the owner closes its subsystem reference.
        pub fn release_after_radio_offline(&mut self) -> Result<()> {
            release_after_offline(&mut self.handle, &mut self.start_attempted,
                std::path::Path::new("/sys/bus/msm_subsys/devices"))
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            if let Some(handle) = self.handle {
                // Emergency best effort only. Never stop all handles, and
                // never turn this unverified fallback into a success receipt.
                let _ = self.client.request(0x21, &[(1, &handle.to_le_bytes())], Duration::from_secs(2), |_| Ok(()));
                eprintln!("owned cellular session dropped without verified disconnect");
            } else if self.start_attempted {
                eprintln!("cellular session dropped with an unknown Start outcome; no wildcard Stop sent");
            }
        }
    }
}

#[cfg(target_os = "android")]
pub use runtime::Session;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asynchronous_setup_observes_paused_states_without_claiming_connection() {
        use std::{cell::Cell, collections::VecDeque, time::Duration};
        let mut replies = VecDeque::from([PacketState::Authenticating, PacketState::Suspended, PacketState::Connected]);
        let mut observed = Vec::new();
        let left = Cell::new(Duration::from_secs(1));
        wait_connected_with(|timeout| {
            assert!(timeout <= left.get() && timeout <= Duration::from_secs(3));
            Ok(replies.pop_front().expect("unexpected state request"))
        }, || left.get(), |d| left.set(left.get().saturating_sub(d)), |s| observed.push(s)).unwrap();
        assert_eq!(observed, [PacketState::Authenticating, PacketState::Suspended, PacketState::Connected]);
        assert!(replies.is_empty());
        assert_eq!(left.get(), Duration::from_millis(600));
        for state in [PacketState::Authenticating, PacketState::Suspended] {
            let left = Cell::new(Duration::from_millis(350));
            let error = wait_connected_with(|timeout| { assert!(timeout <= left.get()); Ok(state) },
                || left.get(), |d| left.set(left.get().saturating_sub(d)), |_| {}).unwrap_err();
            assert!(error.to_string().contains("deadline") && error.to_string().contains("ownership retained"));
            assert!(left.get().is_zero());
        }
        let error = wait_connected_with(|_| Ok(PacketState::Disconnected), || Duration::from_secs(1),
            |_| panic!("explicit disconnect must not be retried"), |_| {}).unwrap_err();
        assert!(error.to_string().contains("explicitly disconnected"));
        let error = wait_connected_with(|_| Err(anyhow::anyhow!("unknown state 99")), || Duration::from_secs(1),
            |_| panic!("unknown protocol response must not be readiness"), |_| panic!("failed query has no state")).unwrap_err();
        assert!(error.to_string().contains("unknown state 99"));
    }

    #[test]
    fn timed_out_and_malformed_start_retain_ownership_and_refuse_another_start() {
        for reply in [Err(anyhow::anyhow!("injected Start timeout")), Ok(0), Ok(u32::MAX)] {
            let (mut handle, mut attempted) = (None, false);
            assert!(begin(&mut handle, &mut attempted, || reply).is_err());
            assert!(attempted && handle.is_none(), "the client still owns an unknown Start");
            let mut sent_again = false;
            assert!(begin(&mut handle, &mut attempted, || { sent_again = true; Ok(7) }).is_err());
            assert!(!sent_again, "a retry must confirm the old transaction first");
        }
        let (mut handle, mut attempted) = (None, false);
        begin(&mut handle, &mut attempted, || Ok(7)).unwrap();
        assert_eq!((handle, attempted), (Some(7), true));
    }

    #[test]
    fn only_unambiguous_kernel_offline_releases_known_or_unknown_calls() {
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let f = Fixture(std::env::temp_dir().join(format!("skyward-offline-test-{}-{unique}", std::process::id())));
        std::fs::create_dir(&f.0).unwrap();
        let modem = f.0.join("subsys0");
        std::fs::create_dir(&modem).unwrap();
        std::fs::write(modem.join("name"), "modem\n").unwrap();
        for initial in [None, Some(7)] {
            let (mut handle, mut attempted) = (initial, true);
            for state in ["ONLINE", "OFFLINING", "CRASHED", ""] {
                std::fs::write(modem.join("state"), state).unwrap();
                assert!(release_after_offline(&mut handle, &mut attempted, &f.0).is_err());
                assert_eq!((handle, attempted), (initial, true), "{state} cannot release ownership");
            }
            std::fs::remove_file(modem.join("state")).unwrap();
            assert!(release_after_offline(&mut handle, &mut attempted, &f.0).is_err());
            assert_eq!((handle, attempted), (initial, true));
            std::fs::write(modem.join("state"), "OFFLINE\n").unwrap();
            release_after_offline(&mut handle, &mut attempted, &f.0).unwrap();
            assert_eq!((handle, attempted), (None, false));
        }
        let duplicate = f.0.join("subsys1");
        std::fs::create_dir(&duplicate).unwrap();
        std::fs::write(duplicate.join("name"), "modem\n").unwrap();
        std::fs::write(duplicate.join("state"), "OFFLINE\n").unwrap();
        assert!(require_modem_offline(&f.0).is_err());
        std::fs::remove_dir_all(duplicate).unwrap();
        std::fs::write(modem.join("name"), "adsp\n").unwrap();
        assert!(require_modem_offline(&f.0).is_err());
    }

    fn reply(fields: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![2, 1, 0, 0x2d, 0, 0, 0];
        for (tag, value) in std::iter::once((2, vec![0, 0, 0, 0])).chain(fields.iter().cloned()) {
            out.push(tag); out.extend((value.len() as u16).to_le_bytes()); out.extend(value);
        }
        let len = (out.len() - 7) as u16; out[5..7].copy_from_slice(&len.to_le_bytes()); out
    }
    fn fields() -> Vec<(u8, Vec<u8>)> {
        [(0x1e, 0xc0000202u32), (0x21, 0xfffffffcu32), (0x20, 0xc0000201u32),
            (0x15, 0xc6336401u32), (0x16, 0xc6336401u32), (0x29, 1500u32)]
            .into_iter().map(|(t, n)| (t, n.to_le_bytes().to_vec())).collect()
    }
    fn decode(fields: &[(u8, Vec<u8>)]) -> Result<Settings> {
        let bytes = reply(fields); settings(&Response::decode(&bytes, 1, 0x2d)?)
    }
    #[test]
    fn decodes_little_endian_network_settings_without_duplicate_dns() {
        let result = decode(&fields()).unwrap();
        assert_eq!(result.address, Ipv4Addr::new(192, 0, 2, 2));
        assert_eq!(result.gateway, Ipv4Addr::new(192, 0, 2, 1));
        assert_eq!(result.netmask, Ipv4Addr::new(255, 255, 255, 252));
        assert_eq!(result.dns, [Ipv4Addr::new(198, 51, 100, 1)]);
        assert_eq!(result.mtu, 1500);
    }
    #[test]
    fn rejects_incomplete_malformed_or_unsafe_settings() {
        for tag in [0x1e, 0x21, 0x20, 0x29] {
            let mut f = fields(); f.retain(|(t, _)| *t != tag); assert!(decode(&f).is_err());
        }
        for (tag, value) in [(0x1e, 0), (0x1e, 0xc0000203), (0x21, 0xffff00ff),
            (0x20, 0xcb007101), (0x15, 0x7f000001), (0x29, 0), (0x29, 65_536)] {
            let mut f = fields(); f.iter_mut().find(|(t, _)| *t == tag).unwrap().1 = u32::to_le_bytes(value).to_vec();
            assert!(decode(&f).is_err(), "accepted tag {tag:#x}");
        }
        let mut f = fields(); f.retain(|(t, _)| ![0x15, 0x16].contains(t)); assert!(decode(&f).is_err());
        let mut f = fields(); f[0].1.pop(); assert!(decode(&f).is_err());
    }
    #[test]
    fn only_verified_native_qmapv5_is_accepted() {
        let format = |up, down| DataFormat { link_layer: 2, uplink_aggregation: up,
            downlink_aggregation: down, downlink_max_datagrams: None, downlink_max_size: None };
        assert!(managed_format(&format(Some(9), Some(9))).is_ok());
        for f in [format(Some(5), Some(5)), format(None, None), format(Some(9), Some(5))] {
            assert!(managed_format(&f).is_err());
        }
    }
    #[test]
    fn a_stop_error_and_contradictory_statuses_never_prove_teardown() {
        // The observed NO_EFFECT error must remain an error at the codec
        // boundary; only later packet-status observations can prove closure.
        let mut no_effect = reply(&[]);
        no_effect[10..14].copy_from_slice(&[1, 0, 26, 0]);
        assert!(Response::decode(&no_effect, 1, 0x2d).err().unwrap().to_string().contains("error=26"));
        use PacketState::{Connected, Disconnected, Suspended, Authenticating};
        assert!(!stopped(None, Some(Disconnected)), "lost owning-client response is not closure");
        assert!(!stopped(Some(Disconnected), None), "a failed independent check is not closure");
        assert!(!stopped(Some(Connected), Some(Disconnected)), "contradictory clients retain ownership");
        assert!(!stopped(Some(Disconnected), Some(Authenticating)), "another transition is still active");
        assert!(!stopped(Some(Suspended), Some(Suspended)), "suspended is not disconnected");
        assert!(stopped(Some(Disconnected), Some(Disconnected)), "both observations confirm closure");
    }
    #[test]
    fn status_reply_cannot_be_confused_with_two_byte_indication() {
        for (value, accepted) in [(vec![1], true), (vec![2], true), (vec![3], true),
            (vec![4], true), (vec![0], false), (vec![1, 0], false), (vec![], false)] {
            let bytes = reply(&[(1, value)]); let response = Response::decode(&bytes, 1, 0x2d).unwrap();
            assert_eq!(packet_state(&response).is_ok(), accepted);
        }
    }
}
