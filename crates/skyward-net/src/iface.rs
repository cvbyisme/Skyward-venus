//! A network interface's own settings: up or down, its address, and the
//! default route through it, set the way ifconfig and route set them.

use anyhow::{Context, Result};
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// The kernel's `struct rtentry`, which the C library of a phone does not
/// describe.
#[repr(C)]
struct Route {
    pad1: libc::c_ulong,
    dst: libc::sockaddr,
    gateway: libc::sockaddr,
    genmask: libc::sockaddr,
    flags: libc::c_ushort,
    pad2: libc::c_short,
    pad3: libc::c_ulong,
    pad4: *mut libc::c_void,
    /// The route's metric plus one.
    metric: libc::c_short,
    dev: *mut libc::c_char,
    mtu: libc::c_ulong,
    window: libc::c_ulong,
    irtt: libc::c_ushort,
}

const RTF_UP: libc::c_ushort = 0x1;
const RTF_GATEWAY: libc::c_ushort = 0x2;
const RTF_REJECT: libc::c_ushort = 0x200;

/// The exact IPv4 configuration an owner expects to remain installed.
/// Retain the original ifindex: a replacement with the same name is not
/// the interface this owner configured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ipv4Configuration {
    pub ifindex: u32,
    pub address: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub mtu: u32,
    pub gateway: Ipv4Addr,
    pub metric: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultRoute {
    pub gateway: Ipv4Addr,
    pub metric: u32,
    pub flags: u16,
}

/// A read-only observation, not a promise that another manager cannot
/// change the interface after it was read. No address is a normal state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ipv4State {
    pub ifindex: u32,
    pub up: bool,
    pub address: Option<Ipv4Addr>,
    pub netmask: Option<Ipv4Addr>,
    pub mtu: u32,
    pub default_routes: Vec<DefaultRoute>,
}

impl Ipv4State {
    /// Check the fields owned by the native link manager, without requiring
    /// the separate default route installed by Recovery.
    pub fn verify_native(&self, expected: &Ipv4Configuration) -> Result<()> {
        anyhow::ensure!(self.ifindex == expected.ifindex, "interface was replaced: ifindex {}, expected {}", self.ifindex, expected.ifindex);
        anyhow::ensure!(self.up, "interface is down");
        anyhow::ensure!(self.address == Some(expected.address), "IPv4 address {:?}, expected {}", self.address, expected.address);
        anyhow::ensure!(self.netmask == Some(expected.netmask), "IPv4 netmask {:?}, expected {}", self.netmask, expected.netmask);
        anyhow::ensure!(self.mtu == expected.mtu, "MTU {}, expected {}", self.mtu, expected.mtu);
        Ok(())
    }

    pub fn verify(&self, expected: &Ipv4Configuration) -> Result<()> {
        self.verify_native(expected)?;
        let routes: Vec<_> = self.default_routes.iter().filter(|r| r.metric == u32::from(expected.metric)).collect();
        anyhow::ensure!(routes.len() == 1 && routes[0].gateway == expected.gateway
            && routes[0].flags & (RTF_UP | RTF_GATEWAY | RTF_REJECT) == RTF_UP | RTF_GATEWAY,
            "default routes at metric {} are {:?}, expected active gateway {}", expected.metric, routes, expected.gateway);
        Ok(())
    }
}

fn socket() -> Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("make a socket to set an interface with");
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn request(interface: &str) -> Result<libc::ifreq> {
    let mut r: libc::ifreq = unsafe { std::mem::zeroed() };
    anyhow::ensure!(!interface.is_empty() && !interface.contains('\0'), "invalid interface name");
    anyhow::ensure!(interface.len() < r.ifr_name.len(), "{interface} is too long a name for an interface");
    for (to, from) in r.ifr_name.iter_mut().zip(interface.bytes()) {
        *to = from as libc::c_char;
    }
    Ok(r)
}

fn ioctl(fd: &OwnedFd, what: libc::c_int, arg: *mut libc::c_void, doing: &str) -> Result<()> {
    if unsafe { libc::ioctl(fd.as_raw_fd(), what as _, arg) } != 0 {
        return Err(std::io::Error::last_os_error()).context(doing.to_string());
    }
    Ok(())
}

fn sockaddr(address: Ipv4Addr) -> libc::sockaddr {
    let inet = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr { s_addr: u32::from_ne_bytes(address.octets()) },
        sin_zero: [0; 8],
    };
    unsafe { std::mem::transmute::<libc::sockaddr_in, libc::sockaddr>(inet) }
}

fn ifindex(fd: &OwnedFd, interface: &str) -> Result<u32> {
    let mut r = request(interface)?;
    ioctl(fd, libc::SIOCGIFINDEX as _, (&mut r as *mut libc::ifreq).cast(), &format!("read {interface}'s ifindex"))?;
    let index = unsafe { r.ifr_ifru.ifru_ifindex };
    anyhow::ensure!(index > 0, "{interface} has an invalid ifindex {index}");
    Ok(index as u32)
}

fn read_ipv4_field(fd: &OwnedFd, interface: &str, what: libc::c_int) -> Result<Option<Ipv4Addr>> {
    let mut r = request(interface)?;
    if unsafe { libc::ioctl(fd.as_raw_fd(), what as _, &mut r) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EADDRNOTAVAIL) { return Ok(None); }
        return Err(error).with_context(|| format!("read {interface}'s IPv4 field {what:#x}"));
    }
    let inet = unsafe { std::mem::transmute::<libc::sockaddr, libc::sockaddr_in>(r.ifr_ifru.ifru_addr) };
    anyhow::ensure!(inet.sin_family == libc::AF_INET as libc::sa_family_t, "{interface} returned a non-IPv4 address");
    Ok(Some(Ipv4Addr::from(inet.sin_addr.s_addr.to_ne_bytes())))
}

/// Read kernel state without bringing the interface up or repairing it.
/// Detect replacement during the read as well as between observations.
/// Callers must keep checking while claiming the configuration is ready.
pub fn read_ipv4(interface: &str) -> Result<Ipv4State> {
    let fd = socket()?;
    let index = ifindex(&fd, interface)?;
    let mut r = request(interface)?;
    ioctl(&fd, libc::SIOCGIFFLAGS as _, (&mut r as *mut libc::ifreq).cast(), &format!("read {interface}'s flags"))?;
    let up = unsafe { r.ifr_ifru.ifru_flags } & libc::IFF_UP as libc::c_short != 0;
    let address = read_ipv4_field(&fd, interface, libc::SIOCGIFADDR as _)?;
    let netmask = read_ipv4_field(&fd, interface, libc::SIOCGIFNETMASK as _)?;
    ioctl(&fd, libc::SIOCGIFMTU as _, (&mut r as *mut libc::ifreq).cast(), &format!("read {interface}'s MTU"))?;
    let mtu = unsafe { r.ifr_ifru.ifru_mtu };
    anyhow::ensure!(mtu > 0, "{interface} has an invalid MTU {mtu}");
    let table = std::fs::read_to_string("/proc/net/route").context("read the interface routes")?;
    let default_routes = table.lines().filter_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 8 || fields[0] != interface
            || u32::from_str_radix(fields[1], 16).ok()? != 0
            || u32::from_str_radix(fields[7], 16).ok()? != 0 { return None; }
        Some(DefaultRoute {
            gateway: Ipv4Addr::from(u32::from_str_radix(fields[2], 16).ok()?.to_ne_bytes()),
            metric: fields[6].parse().ok()?,
            flags: u16::from_str_radix(fields[3], 16).ok()?,
        })
    }).collect();
    anyhow::ensure!(ifindex(&fd, interface)? == index, "{interface} was replaced while reading its configuration");
    Ok(Ipv4State { ifindex: index, up, address, netmask, mtu: mtu as u32, default_routes })
}

pub fn verify_ipv4(interface: &str, expected: &Ipv4Configuration) -> Result<()> {
    read_ipv4(interface)?.verify(expected).with_context(|| format!("verify {interface}'s IPv4 configuration"))
}

/// Wait for a native manager's transition without changing the link. A
/// replacement is never accepted as the original WDS call's generation.
fn wait_native_with(
    index: u32,
    mut read: impl FnMut() -> Result<Ipv4State>,
    ready: impl Fn(&Ipv4State) -> Result<()>,
    remaining: impl Fn() -> std::time::Duration,
    mut sleep: impl FnMut(std::time::Duration),
) -> Result<()> {
    let mut last = "native interface has not been observed".to_string();
    loop {
        anyhow::ensure!(!remaining().is_zero(), "native interface readiness deadline expired: {last}");
        let current = read()?;
        anyhow::ensure!(current.ifindex == index, "native interface was replaced: ifindex {}, expected {index}", current.ifindex);
        match ready(&current) {
            Ok(()) => return Ok(()),
            Err(error) => last = format!("{error:#}"),
        }
        sleep(remaining().min(std::time::Duration::from_millis(100)));
    }
}

/// Require native cleanup before starting a new WDS call. In particular,
/// an old address must never satisfy the next call's readiness check.
pub fn wait_native_idle(interface: &str, timeout: std::time::Duration) -> Result<u32> {
    let index = read_ipv4(interface)?.ifindex;
    let until = std::time::Instant::now() + timeout;
    wait_native_with(index, || read_ipv4(interface), |state| {
        anyhow::ensure!(!state.up && state.address.is_none(),
            "native interface is not idle: up={}, address={:?}", state.up, state.address);
        Ok(())
    }, || until.saturating_duration_since(std::time::Instant::now()), std::thread::sleep)?;
    Ok(index)
}

/// Wait for netmgr's real address, mask and MTU, including its asynchronous
/// KIF address reset/add. No IFF_UP, address, MTU or netmask write is made.
pub fn wait_native_ipv4(interface: &str, expected: &Ipv4Configuration, timeout: std::time::Duration) -> Result<()> {
    let until = std::time::Instant::now() + timeout;
    wait_native_with(expected.ifindex, || read_ipv4(interface), |state| state.verify_native(expected),
        || until.saturating_duration_since(std::time::Instant::now()), std::thread::sleep)
}

fn route_slot(state: &Ipv4State, expected: &Ipv4Configuration) -> Result<bool> {
    anyhow::ensure!(state.ifindex == expected.ifindex, "refuse route operation on replacement interface");
    let routes: Vec<_> = state.default_routes.iter().filter(|r| r.metric == u32::from(expected.metric)).collect();
    if routes.is_empty() { return Ok(false); }
    anyhow::ensure!(routes.len() == 1 && routes[0].gateway == expected.gateway
        && routes[0].flags == (RTF_UP | RTF_GATEWAY),
        "foreign or ambiguous default route at owned metric {}: {:?}", expected.metric, routes);
    Ok(true)
}

/// Add or delete exactly one owned route, without touching native IP or
/// another interface/metric. Metric zero is forbidden here because Linux's
/// legacy delete API treats it as a wildcard.
fn owned_route_ioctl(interface: &str, expected: &Ipv4Configuration, add: bool) -> Result<()> {
    anyhow::ensure!(expected.metric != 0, "owned route cannot use wildcard metric zero");
    let metric = i16::try_from(u32::from(expected.metric) + 1).context("route metric exceeds the ioctl range")?;
    let fd = socket()?;
    anyhow::ensure!(ifindex(&fd, interface)? == expected.ifindex, "refuse route operation on replacement interface");
    let mut name = request(interface)?.ifr_name;
    let mut route = Route {
        pad1: 0, dst: sockaddr(Ipv4Addr::UNSPECIFIED), gateway: sockaddr(expected.gateway),
        genmask: sockaddr(Ipv4Addr::UNSPECIFIED), flags: RTF_UP | RTF_GATEWAY,
        pad2: 0, pad3: 0, pad4: std::ptr::null_mut(), metric, dev: name.as_mut_ptr(),
        mtu: 0, window: 0, irtt: 0,
    };
    let what = if add { libc::SIOCADDRT } else { libc::SIOCDELRT };
    if unsafe { libc::ioctl(fd.as_raw_fd(), what as _, (&mut route as *mut Route).cast::<libc::c_void>()) } != 0 {
        let error = std::io::Error::last_os_error();
        // The native address teardown may have already removed our route.
        // Never convert EEXIST on add into adoption of a foreign route.
        if add || error.raw_os_error() != Some(libc::ESRCH) { return Err(error).context("change owned default route"); }
    }
    Ok(())
}

/// Success means this call installed the route. Retain `expected` before
/// subsequent verification so an error cannot forget the route's owner.
pub fn add_owned_default(interface: &str, expected: &Ipv4Configuration) -> Result<()> {
    let state = read_ipv4(interface)?;
    state.verify_native(expected)?;
    anyhow::ensure!(!route_slot(&state, expected)?, "default route already exists; refusing to adopt it");
    owned_route_ioctl(interface, expected, true)
}

/// Remove only a previously installed exact route. An already absent route
/// is confirmed success, while a different route is left untouched.
pub fn remove_owned_default(interface: &str, expected: &Ipv4Configuration) -> Result<()> {
    let state = read_ipv4(interface)?;
    if route_slot(&state, expected)? { owned_route_ioctl(interface, expected, false)?; }
    anyhow::ensure!(!route_slot(&read_ipv4(interface)?, expected)?, "owned default route remains after deletion");
    Ok(())
}

/// The interface's hardware address.
pub fn hardware_address(interface: &str) -> Result<[u8; 6]> {
    let fd = socket()?;
    let mut r = request(interface)?;
    ioctl(&fd, libc::SIOCGIFHWADDR as libc::c_int, (&mut r as *mut libc::ifreq).cast(), &format!("read the hardware address of {interface}"))?;
    let data = unsafe { r.ifr_ifru.ifru_hwaddr.sa_data };
    let mut mac = [0u8; 6];
    // A C char is signed on some phones and not on others.
    for (to, from) in mac.iter_mut().zip(data) {
        *to = from.to_ne_bytes()[0];
    }
    Ok(mac)
}

/// Bring the interface up, or take it down.
pub fn set_up(interface: &str, up: bool) -> Result<()> {
    let fd = socket()?;
    let mut r = request(interface)?;
    ioctl(&fd, libc::SIOCGIFFLAGS as libc::c_int, (&mut r as *mut libc::ifreq).cast(), &format!("read how {interface} stands"))?;
    unsafe {
        if up {
            r.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short;
        } else {
            r.ifr_ifru.ifru_flags &= !(libc::IFF_UP as libc::c_short);
        }
    }
    let doing = if up { format!("bring {interface} up") } else { format!("take {interface} down") };
    ioctl(&fd, libc::SIOCSIFFLAGS as libc::c_int, (&mut r as *mut libc::ifreq).cast(), &doing)
}

/// Apply the IPv4 MTU negotiated for this interface. Validate before the
/// ioctl; the driver also enforces its own supported range.
pub fn set_mtu(interface: &str, mtu: u32) -> Result<()> {
    anyhow::ensure!((68..=65_535).contains(&mtu), "{mtu} is not an IPv4 MTU");
    let fd = socket()?;
    let mut r = request(interface)?;
    r.ifr_ifru.ifru_mtu = mtu as libc::c_int;
    ioctl(&fd, libc::SIOCSIFMTU as libc::c_int, (&mut r as *mut libc::ifreq).cast(), &format!("set {interface}'s MTU to {mtu}"))
}

/// Give the interface `address` in a network of `mask`; what is sent to
/// any other goes through `router`.
pub fn configure(interface: &str, address: Ipv4Addr, mask: Ipv4Addr, router: Option<Ipv4Addr>) -> Result<()> {
    configure_with_metric(interface, address, mask, router, 0)
}

/// Like `configure`, with a route preference: lower metrics are preferred.
/// Only defaults belonging to this interface and metric are replaced.
pub fn configure_with_metric(interface: &str, address: Ipv4Addr, mask: Ipv4Addr, router: Option<Ipv4Addr>, metric: u16) -> Result<()> {
    // The legacy rtentry API carries metric + 1 in a signed short.
    let route_metric = i16::try_from(u32::from(metric) + 1).context("route metric exceeds the ioctl range")?;
    let fd = socket()?;
    let mut r = request(interface)?;
    r.ifr_ifru.ifru_addr = sockaddr(address);
    ioctl(&fd, libc::SIOCSIFADDR as libc::c_int, (&mut r as *mut libc::ifreq).cast(), &format!("give {interface} the address {address}"))?;
    r.ifr_ifru.ifru_netmask = sockaddr(mask);
    ioctl(&fd, libc::SIOCSIFNETMASK as libc::c_int, (&mut r as *mut libc::ifreq).cast(), &format!("give {interface} the mask {mask}"))?;
    let mut name = r.ifr_name;
    let device = name.as_mut_ptr();
    let make_route = |gateway| Route {
        pad1: 0,
        dst: sockaddr(Ipv4Addr::UNSPECIFIED),
        gateway: sockaddr(gateway),
        genmask: sockaddr(Ipv4Addr::UNSPECIFIED),
        flags: RTF_UP | if gateway == Ipv4Addr::UNSPECIFIED { 0 } else { RTF_GATEWAY },
        pad2: 0,
        pad3: 0,
        pad4: std::ptr::null_mut(),
        metric: route_metric,
        dev: device,
        mtu: 0,
        window: 0,
        irtt: 0,
    };
    // Read the previous gateways, not the new gateway. In particular a
    // metric-zero delete is a kernel wildcard, so never issue one unless
    // this interface really has a default with that metric.
    let table = std::fs::read_to_string("/proc/net/route").context("read the interface routes")?;
    for gateway in default_gateways(&table, interface, metric) {
        let mut route = make_route(gateway);
        if unsafe { libc::ioctl(fd.as_raw_fd(), libc::SIOCDELRT as _, (&mut route as *mut Route).cast::<libc::c_void>()) } != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ESRCH) {
                return Err(e).with_context(|| format!("take away the default route through {interface} with metric {metric}"));
            }
        }
    }
    let Some(router) = router else { return Ok(()) };
    let mut route = make_route(router);
    ioctl(&fd, libc::SIOCADDRT as libc::c_int, (&mut route as *mut Route).cast(), &format!("route through {router} on {interface}"))
}

fn default_gateways(table: &str, interface: &str, metric: u16) -> Vec<Ipv4Addr> {
    table.lines().filter_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 8 || fields[0] != interface
            || u32::from_str_radix(fields[1], 16).ok()? != 0
            || u32::from_str_radix(fields[7], 16).ok()? != 0
            || fields[6].parse::<u16>().ok()? != metric
            || u16::from_str_radix(fields[3], 16).ok()? & RTF_UP == 0 { return None; }
        Some(Ipv4Addr::from(u32::from_str_radix(fields[2], 16).ok()?.to_ne_bytes()))
    }).collect()
}

/// Take the interface's address away, and the routes through it with it.
pub fn unconfigure(interface: &str) -> Result<()> {
    let fd = socket()?;
    let mut r = request(interface)?;
    r.ifr_ifru.ifru_addr = sockaddr(Ipv4Addr::UNSPECIFIED);
    ioctl(&fd, libc::SIOCSIFADDR as libc::c_int, (&mut r as *mut libc::ifreq).cast(), &format!("take the address of {interface} away"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native_fixture() -> (Ipv4Configuration, Ipv4State) {
        let expected = Ipv4Configuration {
            ifindex: 7, address: Ipv4Addr::new(192, 0, 2, 2), netmask: Ipv4Addr::new(255, 255, 255, 0),
            mtu: 1500, gateway: Ipv4Addr::new(192, 0, 2, 1), metric: 100,
        };
        let observed = Ipv4State {
            ifindex: 7, up: true, address: Some(expected.address), netmask: Some(expected.netmask), mtu: 1500,
            default_routes: Vec::new(),
        };
        (expected, observed)
    }

    #[test]
    fn native_readiness_waits_for_actual_kif_configuration_and_is_bounded() {
        use std::{cell::Cell, collections::VecDeque, time::Duration};
        let (expected, ready) = native_fixture();
        let mut cleared = ready.clone(); cleared.address = None; cleared.netmask = None;
        let mut partial = ready.clone(); partial.netmask = Some(Ipv4Addr::new(255,0,0,0));
        let mut down = cleared.clone(); down.up = false;
        let mut reads = VecDeque::from([down, cleared.clone(), partial, ready.clone()]);
        let left = Cell::new(Duration::from_secs(1));
        wait_native_with(7, || Ok(reads.pop_front().expect("unexpected read")), |s| s.verify_native(&expected),
            || left.get(), |d| left.set(left.get().saturating_sub(d))).unwrap();
        assert!(reads.is_empty());
        assert_eq!(left.get(), Duration::from_millis(700));
        // An IP must not be accepted on a replacement, despite all fields matching.
        let mut replaced = ready.clone(); replaced.ifindex = 8;
        assert!(wait_native_with(7, || Ok(replaced.clone()), |s| s.verify_native(&expected),
            || Duration::from_secs(1), |_| panic!("replacement must fail immediately")).is_err());
        let left = Cell::new(Duration::from_millis(250));
        let error = wait_native_with(7, || Ok(cleared.clone()), |s| s.verify_native(&expected),
            || left.get(), |d| left.set(left.get().saturating_sub(d))).unwrap_err();
        assert!(error.to_string().contains("deadline") && error.to_string().contains("address"));
        assert!(left.get().is_zero());
        assert!(wait_native_with(7, || Err(anyhow::anyhow!("read failed")), |_| Ok(()),
            || Duration::from_secs(1), |_| panic!("I/O errors cannot become readiness")).is_err());
        assert!(ready.verify_native(&expected).is_ok());
        assert!(ready.verify(&expected).is_err(), "native readiness cannot claim a default route");
    }

    #[test]
    fn owned_route_selection_never_matches_other_metrics_or_foreign_routes() {
        let (expected, mut state) = native_fixture();
        state.default_routes.push(DefaultRoute {gateway: expected.gateway, metric: 0, flags: RTF_UP | RTF_GATEWAY});
        assert!(!route_slot(&state, &expected).unwrap());
        state.default_routes.push(DefaultRoute {gateway: expected.gateway, metric: 100, flags: RTF_UP | RTF_GATEWAY});
        assert!(route_slot(&state, &expected).unwrap());
        state.default_routes[1].gateway = Ipv4Addr::new(192,0,2,9);
        assert!(route_slot(&state, &expected).is_err());
        state.default_routes[1].gateway = expected.gateway;
        state.default_routes[1].flags |= RTF_REJECT;
        assert!(route_slot(&state, &expected).is_err());
        state.default_routes[1].flags = RTF_UP | RTF_GATEWAY;
        state.default_routes.push(state.default_routes[1].clone());
        assert!(route_slot(&state, &expected).is_err());
        state.default_routes.clear(); state.ifindex = 8;
        assert!(route_slot(&state, &expected).is_err(), "absence on a replacement is not ownership proof");
    }

    #[test]
    #[ignore = "requires unshare --user --map-root-user --net and ip"]
    fn isolated_native_ipv4_route_ownership_does_not_mutate_address_or_link() {
        let before = std::env::var("SKYWARD_TEST_OUTER_NETNS").expect("set the outer network namespace identity");
        assert_ne!(std::fs::read_link("/proc/self/ns/net").unwrap().to_string_lossy(), before);
        let ip = |args: &[&str]| {
            let out = std::process::Command::new("ip").args(args).output().unwrap();
            assert!(out.status.success(), "ip {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        ip(&["link", "add", "snative0", "type", "dummy"]);
        let index = wait_native_idle("snative0", std::time::Duration::from_secs(1)).unwrap();
        ip(&["link", "set", "snative0", "up"]);
        ip(&["addr", "add", "192.0.2.2/24", "dev", "snative0"]);
        ip(&["route", "add", "default", "via", "192.0.2.9", "dev", "snative0", "metric", "50"]);
        let (mut expected, _) = native_fixture(); expected.ifindex = index;
        wait_native_ipv4("snative0", &expected, std::time::Duration::from_secs(1)).unwrap();
        let native = read_ipv4("snative0").unwrap();
        add_owned_default("snative0", &expected).unwrap();
        verify_ipv4("snative0", &expected).unwrap();
        assert!(add_owned_default("snative0", &expected).is_err(), "cannot adopt EEXIST");
        remove_owned_default("snative0", &expected).unwrap();
        remove_owned_default("snative0", &expected).unwrap();
        assert_eq!(read_ipv4("snative0").unwrap(), native, "route removal cannot change native address, MTU, flags or other metric");
        ip(&["route", "add", "default", "via", "192.0.2.9", "dev", "snative0", "metric", "100"]);
        assert!(add_owned_default("snative0", &expected).is_err());
        assert!(remove_owned_default("snative0", &expected).is_err());
        ip(&["route", "del", "default", "via", "192.0.2.9", "dev", "snative0", "metric", "100"]);
        add_owned_default("snative0", &expected).unwrap();
        // Native teardown removes the route as a kernel consequence.
        ip(&["addr", "del", "192.0.2.2/24", "dev", "snative0"]);
        remove_owned_default("snative0", &expected).unwrap();
        assert!(read_ipv4("snative0").unwrap().up, "route cleanup cannot take native link down");
        ip(&["link", "del", "snative0"]);
        ip(&["link", "add", "snative0", "type", "dummy"]);
        assert!(remove_owned_default("snative0", &expected).is_err());
        ip(&["link", "del", "snative0"]);
    }

    #[test]
    fn route_replacement_selects_only_its_interface_and_metric() {
        let gateway = u32::from_ne_bytes([192, 0, 2, 1]);
        let table = format!("Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT\n\
            wlan0 00000000 {gateway:08X} 0003 0 0 0 00000000 0 0 0\n\
            wlan0 00000000 {gateway:08X} 0003 0 0 100 00000000 0 0 0\n\
            rmnet0 00000000 {gateway:08X} 0003 0 0 0 00000000 0 0 0\n\
            wlan0 00000000 {gateway:08X} 0003 0 0 0 00FFFFFF 0 0 0\n\
            wlan0 00000000 {gateway:08X} 0000 0 0 0 00000000 0 0 0\n");
        assert_eq!(default_gateways(&table, "wlan0", 0), vec![Ipv4Addr::new(192, 0, 2, 1)]);
        assert_eq!(default_gateways(&table, "wlan0", 100), vec![Ipv4Addr::new(192, 0, 2, 1)]);
        assert!(default_gateways(&table, "wlan0", 50).is_empty());
        assert!(default_gateways(&table, "wlan", 0).is_empty());
    }

    #[test]
    fn invalid_interface_and_mtu_are_rejected_before_configuration() {
        for name in ["", "wlan0\0other", "an-interface-name-that-is-too-long"] { assert!(request(name).is_err()); }
        for mtu in [0, 67, 65_536, u32::MAX] { assert!(set_mtu("does-not-exist", mtu).is_err()); }
        assert!(configure_with_metric("does-not-exist", Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED, None, u16::MAX).is_err());
    }

    #[test]
    fn verification_requires_the_exact_active_gateway_and_metric() {
        let expected = Ipv4Configuration {
            ifindex: 7, address: Ipv4Addr::new(192, 0, 2, 2), netmask: Ipv4Addr::new(255, 255, 255, 0),
            mtu: 1500, gateway: Ipv4Addr::new(192, 0, 2, 1), metric: 100,
        };
        let mut observed = Ipv4State {
            ifindex: 7, up: true, address: Some(expected.address), netmask: Some(expected.netmask), mtu: 1500,
            default_routes: vec![DefaultRoute { gateway: expected.gateway, metric: 100, flags: RTF_UP | RTF_GATEWAY }],
        };
        observed.verify(&expected).unwrap();
        observed.default_routes[0].metric = 0;
        assert!(observed.verify(&expected).is_err());
        observed.default_routes[0].metric = 100;
        for flags in [0, RTF_UP, RTF_UP | RTF_GATEWAY | RTF_REJECT] {
            observed.default_routes[0].flags = flags;
            assert!(observed.verify(&expected).is_err());
        }
        observed.default_routes[0].flags = RTF_UP | RTF_GATEWAY;
        observed.default_routes.push(observed.default_routes[0].clone());
        assert!(observed.verify(&expected).is_err(), "an ambiguous equal-metric route is not verified");
    }

    /// Run only inside a fresh, throwaway user/network namespace. The
    /// caller supplies its old namespace identity, which must differ.
    #[test]
    #[ignore = "requires unshare --user --map-root-user --net and ip"]
    fn isolated_routes_and_mtu_preserve_the_fallback_interface() {
        let before = std::env::var("SKYWARD_TEST_OUTER_NETNS").expect("set the outer network namespace identity");
        let here = std::fs::read_link("/proc/self/ns/net").unwrap();
        assert_ne!(here.to_string_lossy(), before, "never exercise host interfaces");
        let ip = |args: &[&str]| {
            let output = std::process::Command::new("ip").args(args).output().unwrap();
            assert!(output.status.success(), "ip {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        };
        ip(&["link", "add", "swlan0", "type", "dummy"]);
        ip(&["link", "add", "scell0", "type", "dummy"]);
        for interface in ["swlan0", "scell0"] { set_up(interface, true).unwrap(); }
        let wifi = Ipv4Addr::new(192, 0, 2, 2);
        let old = Ipv4Addr::new(192, 0, 2, 1);
        let new = Ipv4Addr::new(192, 0, 2, 3);
        let cell = Ipv4Addr::new(198, 51, 100, 2);
        let fallback = Ipv4Addr::new(198, 51, 100, 1);
        let mask = Ipv4Addr::new(255, 255, 255, 0);
        configure_with_metric("scell0", cell, mask, Some(fallback), 100).unwrap();
        configure("swlan0", wifi, mask, Some(old)).unwrap();
        ip(&["route", "add", "default", "via", "192.0.2.1", "dev", "swlan0", "metric", "50"]);
        let routes = || std::fs::read_to_string("/proc/net/route").unwrap();
        configure("swlan0", wifi, mask, Some(new)).unwrap();
        assert_eq!(default_gateways(&routes(), "swlan0", 0), [new]);
        assert_eq!(default_gateways(&routes(), "swlan0", 50), [old]);
        assert_eq!(default_gateways(&routes(), "scell0", 100), [fallback]);
        configure("swlan0", wifi, mask, None).unwrap();
        // Repeating with no metric-zero route must not delete metric 50.
        configure("swlan0", wifi, mask, None).unwrap();
        assert!(default_gateways(&routes(), "swlan0", 0).is_empty());
        assert_eq!(default_gateways(&routes(), "swlan0", 50), [old]);
        assert_eq!(default_gateways(&routes(), "scell0", 100), [fallback]);
        set_mtu("scell0", 1428).unwrap();
        assert!(set_mtu("scell0", 67).is_err());
        let fd = socket().unwrap();
        let mut r = request("scell0").unwrap();
        ioctl(&fd, libc::SIOCGIFMTU as _, (&mut r as *mut libc::ifreq).cast(), "read test MTU").unwrap();
        assert_eq!(unsafe { r.ifr_ifru.ifru_mtu }, 1428);
        let expected = Ipv4Configuration {
            ifindex: read_ipv4("scell0").unwrap().ifindex,
            address: cell, netmask: mask, mtu: 1428, gateway: fallback, metric: 100,
        };
        verify_ipv4("scell0", &expected).unwrap();
        let unchanged = read_ipv4("scell0").unwrap();
        verify_ipv4("scell0", &expected).unwrap();
        assert_eq!(read_ipv4("scell0").unwrap(), unchanged, "verification must not change kernel state");

        set_mtu("scell0", 1400).unwrap();
        assert!(format!("{:#}", verify_ipv4("scell0", &expected).unwrap_err()).contains("MTU"));
        set_mtu("scell0", expected.mtu).unwrap();
        ip(&["route", "del", "default", "via", "198.51.100.1", "dev", "scell0", "metric", "100"]);
        assert!(format!("{:#}", verify_ipv4("scell0", &expected).unwrap_err()).contains("default routes"));
        // A matching route on a different interface cannot satisfy this one.
        assert_eq!(default_gateways(&routes(), "swlan0", 50), [old]);
        configure_with_metric("scell0", cell, mask, Some(Ipv4Addr::new(198, 51, 100, 3)), 100).unwrap();
        assert!(verify_ipv4("scell0", &expected).is_err(), "wrong gateway");
        configure_with_metric("scell0", Ipv4Addr::new(198, 51, 100, 4), mask, Some(fallback), 100).unwrap();
        assert!(format!("{:#}", verify_ipv4("scell0", &expected).unwrap_err()).contains("IPv4 address"));
        configure_with_metric("scell0", cell, Ipv4Addr::new(255, 255, 255, 128), Some(fallback), 100).unwrap();
        assert!(format!("{:#}", verify_ipv4("scell0", &expected).unwrap_err()).contains("netmask"));
        configure_with_metric("scell0", cell, mask, Some(fallback), 100).unwrap();
        set_up("scell0", false).unwrap();
        assert!(format!("{:#}", verify_ipv4("scell0", &expected).unwrap_err()).contains("down"));
        set_up("scell0", true).unwrap();
        configure_with_metric("scell0", cell, mask, Some(fallback), 100).unwrap();
        unconfigure("scell0").unwrap();
        assert_eq!(read_ipv4("scell0").unwrap().address, None);
        assert!(verify_ipv4("scell0", &expected).is_err(), "withdrawn address");
        ip(&["link", "delete", "scell0"]);
        ip(&["link", "add", "scell0", "type", "dummy"]);
        set_up("scell0", true).unwrap();
        set_mtu("scell0", expected.mtu).unwrap();
        configure_with_metric("scell0", cell, mask, Some(fallback), 100).unwrap();
        assert!(format!("{:#}", verify_ipv4("scell0", &expected).unwrap_err()).contains("replaced"));
        verify_ipv4("scell0", &Ipv4Configuration { ifindex: read_ipv4("scell0").unwrap().ifindex, ..expected }).unwrap();
        assert_eq!(default_gateways(&routes(), "swlan0", 50), [old], "other interface's route survives all checks");
        unconfigure("swlan0").unwrap();
        assert_eq!(default_gateways(&routes(), "scell0", 100), [fallback]);
        ip(&["link", "delete", "swlan0"]);
        ip(&["link", "delete", "scell0"]);
    }
}
