//! Device-only, explicit temporary data-call probe. Not started by the UI.
//! Uses this tree's codecs; all handles are owned by this process.
#[cfg(target_os = "android")]
#[path = "../src/qmi.rs"]
#[allow(dead_code)]
mod qmi;
#[cfg(target_os = "android")]
#[path = "../src/qrtr.rs"]
#[allow(dead_code)]
mod qrtr;

#[cfg(target_os = "android")]
mod device {
    use super::{qmi, qrtr};
    use anyhow::{ensure, Context, Result};
    use std::net::Ipv4Addr;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    static END: AtomicBool = AtomicBool::new(false);
    extern "C" fn finish(_: libc::c_int) { END.store(true,Ordering::Relaxed); }
    const WAIT: Duration = Duration::from_secs(10);

    fn extended(command: u32) -> Result<[u32;8]> {
        let fd = unsafe { libc::socket(libc::AF_INET,libc::SOCK_DGRAM|libc::SOCK_CLOEXEC,0) };
        ensure!(fd >= 0,"open rmnet socket: {}",std::io::Error::last_os_error());
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
        for (out,byte) in request.ifr_name.iter_mut().zip(b"rmnet_ipa0") { *out = *byte as libc::c_char; }
        // Exact Venus downstream UAPI size was checked with sizeof(C struct).
        let mut data = [0u32;8]; data[0] = command;
        request.ifr_ifru.ifru_data = data.as_mut_ptr().cast();
        ensure!(unsafe { libc::ioctl(fd.as_raw_fd(),0x89fd,&mut request) } == 0,
            "rmnet ioctl {command}: {}",std::io::Error::last_os_error());
        Ok(data)
    }
    fn endpoint(services: &[qrtr::Endpoint],service: u32) -> Result<qrtr::Endpoint> {
        let found: Vec<_> = services.iter().copied().filter(|e| e.service == service && e.instance == 0 && e.node == 0).collect();
        ensure!(found.len() == 1,"expected one primary QMI service {service}"); Ok(found[0])
    }
    struct Call { wds: qrtr::Client, dpm: Option<qrtr::Client>, handle: Option<u32> }
    impl Drop for Call {
        fn drop(&mut self) {
            if let Some(handle) = self.handle.take() {
                eprintln!("WDS_STOP {:?}",self.wds.request(0x21,&[(1,&handle.to_le_bytes())],WAIT,|_| Ok(())));
            }
            if let Some(dpm) = &mut self.dpm { eprintln!("DPM_CLOSE {:?}",dpm.request(0x21,&[],WAIT,|_| Ok(()))); }
        }
    }
    pub fn run() -> Result<()> {
        let args: Vec<_> = std::env::args().collect();
        ensure!(matches!(args.get(1).map(String::as_str),Some("inspect"|"connect"|"connect-managed")),"usage: venus_cellular_probe inspect|connect|connect-managed [APN]");
        let managed = args[1] == "connect-managed";
        ensure!(unsafe { libc::geteuid() } == 0,"requires root to read the IPA endpoint");
        ensure!(args[1] == "inspect" || std::path::Path::new("/skyward/venus-recovery.txt").is_file(),"data calls require temporary Venus Recovery; inspect is read-only");
        unsafe { libc::signal(libc::SIGTERM,finish as *const () as libc::sighandler_t); libc::signal(libc::SIGINT,finish as *const () as libc::sighandler_t); }
        let services = qrtr::discover()?;
        let epid = extended(3)?[1]; let pair = extended(16)?;
        let ep: Vec<_> = [4u32,epid].into_iter().flat_map(u32::to_le_bytes).collect();
        println!("IPA endpoint={epid} consumer={} producer={}",pair[1],pair[2]);
        let mut wda = qrtr::Client::new(endpoint(&services,26)?)?;
        eprintln!("WDA_BEFORE {:?}",wda.request(0x21,&[(0x10,&ep)],WAIT,qmi::data_format));
        if args[1] == "inspect" {
            eprintln!("NAS_HOME_NETWORK {:?}",qrtr::status(endpoint(&services,3)?,qmi::StatusRequest::HomeNetwork,qmi::home_network));
            if !std::path::Path::new("/sys/class/net/rmnet_data0").exists() {
                eprintln!("WDS_PACKET_STATUS unavailable: vendor data channel is not configured");
                return Ok(())
            }
            let mut wds = qrtr::Client::new(endpoint(&services,1)?)?;
            wds.request(0xaf,&[(1,&1u32.to_le_bytes())],WAIT,|_| Ok(()))?;
            wds.request(0xa2,&[(0x10,&ep),(0x11,&[1]),(0x13,&1u32.to_le_bytes())],WAIT,|_| Ok(()))?;
            eprintln!("WDS_PACKET_STATUS {:?}",wds.request(0x22,&[],WAIT,|r| Ok(r.required(1)?.to_vec())));
            return Ok(())
        }
        let reg = qrtr::status(endpoint(&services,3)?,qmi::StatusRequest::ServingSystem,qmi::serving_system)?;
        ensure!(reg.registration_state == 1 && reg.packet_attach_state == 1,"modem is not packet registered");
        let mut call = Call {
            wds: qrtr::Client::new(endpoint(&services,1)?)?,
            dpm: None, handle: None,
        };
        if managed {
            ensure!(std::path::Path::new("/sys/class/net/rmnet_data0").exists(),"vendor netmgr has not created its data channel");
            let format = wda.request(0x21,&[(0x10,&ep)],WAIT,qmi::data_format)?;
            ensure!(format.link_layer == 2 && format.uplink_aggregation == Some(9) && format.downlink_aggregation == Some(9),"vendor data format differs from the measured normal-system QMAPv5 profile: {format:?}");
            eprintln!("VENDOR_MANAGED_FORMAT {format:?}");
        } else {
        call.dpm = Some(qrtr::Client::new(endpoint(&services,47)?)?);
        let hardware: Vec<_> = std::iter::once(1u8).chain([4u32,epid,pair[1],pair[2]].into_iter().flat_map(u32::to_le_bytes)).collect();
        call.dpm.as_mut().context("missing owned DPM client")?.request(0x20,&[(0x11,&hardware)],WAIT,|_| Ok(())).context("open embedded hardware data port")?;
        eprintln!("DPM_OPEN ok");
        let format = wda.request(0x20,&[
            (0x10,&[0]), (0x11,&2u32.to_le_bytes()), (0x12,&5u32.to_le_bytes()),
            (0x13,&5u32.to_le_bytes()), (0x15,&1u32.to_le_bytes()),
            (0x16,&16384u32.to_le_bytes()), (0x17,&ep),
            (0x1b,&1u32.to_le_bytes()), (0x1c,&16384u32.to_le_bytes()),
        ],WAIT,qmi::data_format).context("configure embedded raw-IP QMAP")?;
        ensure!(format.link_layer == 2 && format.uplink_aggregation == Some(5) && format.downlink_aggregation == Some(5),"unexpected negotiated format {format:?}");
        eprintln!("WDA_CONFIG {format:?}");
        }
        call.wds.request(0xaf,&[(1,&1u32.to_le_bytes())],WAIT,|_| Ok(())).context("bind primary subscription")?;
        call.wds.request(0xa2,&[(0x10,&ep),(0x11,&[1]),(0x13,&1u32.to_le_bytes())],WAIT,|_| Ok(())).context("bind QMAP mux one")?;
        call.wds.request(0x4d,&[(1,&[4])],WAIT,|_| Ok(())).context("select IPv4")?;
        let profile = call.wds.request(0x49,&[(1,&[0,0])],WAIT,|r| {
            let field = r.required(1)?; ensure!(field.len() == 1,"invalid default profile size"); Ok(field[0])
        }).context("read default 3GPP profile")?;
        eprintln!("DEFAULT_PROFILE {profile}");
        let mut fields: Vec<(u8,&[u8])> = vec![(0x19,&[4])];
        let profile_field = [profile];
        if let Some(apn) = args.get(2) {
            ensure!(!apn.is_empty() && apn.len() <= 100 && apn.bytes().all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b)),"invalid APN");
            fields.push((0x14,apn.as_bytes()));
        } else { fields.push((0x31,&profile_field)); }
        call.handle = Some(call.wds.request(0x20,&fields,Duration::from_secs(45),|r| {
            Ok(u32::from_le_bytes(r.required(1)?.try_into().context("invalid data handle")?))
        }).context("start IPv4 call")?);
        eprintln!("WDS_START ok handle={:?}",call.handle);
        let settings = call.wds.request(0x2d,&[(0x10,&0x2310u32.to_le_bytes())],WAIT,|r| {
            let address = |tag| -> Result<Option<Ipv4Addr>> {
                r.optional(tag).map(|b| -> Result<_> { Ok(Ipv4Addr::from(u32::from_le_bytes(b.try_into().context("invalid IPv4 field")?))) }).transpose()
            };
            Ok(serde_json::json!({"address":address(0x1e)?,"gateway":address(0x20)?,"netmask":address(0x21)?,"dns":[address(0x15)?,address(0x16)?],
                "mtu":r.optional(0x29).map(|b| -> Result<u32> { Ok(u32::from_le_bytes(b.try_into().context("invalid MTU field")?)) }).transpose()?}))
        }).context("read IPv4 call settings")?;
        println!("SETTINGS {}",serde_json::to_string(&settings)?);
        let until = Instant::now()+Duration::from_secs(600);
        while !END.load(Ordering::Relaxed) && Instant::now() < until { std::thread::sleep(Duration::from_millis(200)); }
        Ok(())
    }
}
#[cfg(target_os = "android")]
fn main() -> anyhow::Result<()> { device::run() }
#[cfg(not(target_os = "android"))]
fn main() { eprintln!("This diagnostic requires temporary Venus Recovery."); }
