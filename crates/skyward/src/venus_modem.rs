//! The installed modem's independent init owner. The UI only requests
//! transitions; RMT/TFTP outlive the UI and any in-flight NV transaction.
use anyhow::{ensure, Context, Result};
use skyward_net::{cellular_data::{PacketState, Session, Settings}, iface, modem::{Phase, State}};
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const HOME: &str = "/skyward/run/state/modem";
const COMMAND: &str = "skyward.modem.command";
const IFACE: &str = "rmnet_data0";
const SUSPEND: &str = "android.system.suspend@1.0::ISystemSuspend";
const NETD: &str = "android.system.net.netd@1.1::INetd";
static END: AtomicBool = AtomicBool::new(false);
static REQUEST: AtomicU64 = AtomicU64::new(0);
extern "C" fn finish(_: libc::c_int) { END.store(true, Ordering::Relaxed); }

pub fn ensure_owner() -> Result<()> {
    ensure!(crate::venus_rw::enabled(), "Venus Recovery required");
    match skyward_sys::prop("init.svc.venus_modem").as_deref() {
        Some("running" | "restarting") => Ok(()),
        Some("stopping") => anyhow::bail!("modem owner is stopping"),
        _ => skyward_sys::set_prop("ctl.start", "venus_modem"),
    }
}

pub fn request(command: &str) -> Result<()> { send_request(command).map(|_|()) }

fn send_request(command: &str) -> Result<String> {
    ensure!(crate::venus_rw::enabled(), "Venus Recovery required");
    ensure!(matches!(command,"radio-on"|"radio-off"|"data-on"|"data-off"),"unknown modem request");
    ensure_owner()?;
    let owner=snapshot()?.context("modem owner is initializing; retry shortly")?.owner;
    let stamp=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis();
    let token=format!("{owner}/{}-{stamp}-{}:{command}",std::process::id(),REQUEST.fetch_add(1,Ordering::Relaxed));
    skyward_sys::set_prop(COMMAND,&token)?;
    Ok(token)
}

pub fn snapshot() -> Result<Option<State>> {
    let path=Path::new(HOME).join("state.json");
    let text=match fs::read(&path) { Ok(s)=>s,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(e)=>return Err(e.into()) };
    ensure!(text.len()<=16384,"oversize modem state");
    let s:State=serde_json::from_slice(&text)?;
    ensure!(s.boot_id==fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim(),"stale modem boot identity");
    let live=skyward_sys::prop("init.svc_debug_pid.venus_modem").and_then(|s|s.parse::<u32>().ok());
    ensure!(live==Some(s.owner) && skyward_sys::prop("init.svc.venus_modem").as_deref()==Some("running"),"modem owner has stopped");
    Ok(Some(s))
}

/// Close the packet call, release the kernel reference and wait for actual
/// OFFLINE before the NV helpers stop. Used before reboot and EFS work.
pub fn quiesce() -> Result<()> {
    if !crate::venus_rw::enabled() { return Ok(()) }
    if skyward_sys::prop("init.svc.venus_modem").as_deref()!=Some("running") {
        ensure!(modem_state()?.as_deref()==Some("OFFLINE"),"unowned modem is not confirmed OFFLINE");
        return Ok(())
    }
    let token=send_request("radio-off")?;
    let until=Instant::now()+Duration::from_secs(100);
    loop {
        if let Some(s)=snapshot()? {
            if s.command==token && s.radio==Phase::Off && s.data==Phase::Off { return Ok(()) }
            if s.command==token && s.radio==Phase::Failed { anyhow::bail!("modem shutdown failed: {}",s.issue.unwrap_or_default()) }
        }
        ensure!(Instant::now()<until,"modem shutdown timed out; NV helpers retained");
        thread::sleep(Duration::from_millis(100));
    }
}

fn publish(s:&State) {
    if let Err(error)=write_state(s) {
        let _=skyward_sys::kmsg(&format!("modem state publication failed; owner retained: {error:#}"));
    }
}

fn write_state(s:&State)->Result<()> {
    let p=Path::new(HOME).join("state.new");
    let bytes=serde_json::to_vec(s)?;
    use std::io::Write;
    let mut f=OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(&p)?;
    f.write_all(&bytes)?;drop(f);
    fs::rename(p,Path::new(HOME).join("state.json"))?;
    Ok(())
}

fn modem_state()->Result<Option<String>> {
    for entry in fs::read_dir("/sys/bus/msm_subsys/devices")? {
        let p=entry?.path();
        if fs::read_to_string(p.join("name")).is_ok_and(|s|s.trim()=="modem") {
            return Ok(Some(fs::read_to_string(p.join("state"))?.trim().to_string()))
        }
    }
    Ok(None)
}

fn spawn(role:&str)->Result<Child> {
    let log=OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(Path::new(HOME).join(format!("{role}.log")))?;
    let mut cmd=if role=="holder" {Command::new("/skyward/runtime/modem-hold")} else {let mut c=Command::new("/skyward/runtime/km41-launch");c.arg(role);c};
    cmd.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).spawn().with_context(||format!("start modem helper {role}"))
}

fn stop_child(child:&mut Option<Child>, timeout:Duration)->Result<()> {
    let Some(c)=child.as_mut() else {return Ok(())};
    if c.try_wait()?.is_none() {
        ensure!(unsafe{libc::kill(c.id() as i32,libc::SIGTERM)}==0,"signal owned modem helper");
        let until=Instant::now()+timeout;
        while c.try_wait()?.is_none() {
            ensure!(Instant::now()<until,"owned modem helper did not stop; not killing a possible NV writer");
            thread::sleep(Duration::from_millis(100));
        }
    }
    child.take();Ok(())
}

fn nv_lock()->Result<File> {
    let f=OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open("/skyward/run/state/modem-nv.lock")?;
    ensure!(unsafe{libc::flock(f.as_raw_fd(),libc::LOCK_EX|libc::LOCK_NB)}==0,"modem or EFS work still owns the radio storage");
    Ok(f)
}

pub fn efs_guard()->Result<Option<File>> {
    if !crate::venus_rw::enabled(){return Ok(None)}
    quiesce()?;
    Ok(Some(nv_lock()?))
}

/// Called only by the disposable query process. A stuck Binder peer must
/// never prevent the independent modem owner from processing radio-off.
pub fn suspend_check()->Result<()> {
    service_check(SUSPEND)
}
pub fn netd_check()->Result<()> {
    service_check(NETD)
}
fn service_check(descriptor:&str)->Result<()> {
    ensure!(crate::venus_rw::enabled() && unsafe{libc::geteuid()}==0,"marked Venus Recovery and root required");
    let binder=skyward_binder::driver::Binder::hardware()?;
    let present=if let Some(service)=skyward_binder::hidl::find(&binder,descriptor,"default")? {
        ensure!(skyward_binder::hidl::describes(&service)?==descriptor,"foreign network helper interface");
        true
    } else {false};
    println!("{}",if present {"present"} else {"absent"});
    Ok(())
}

#[derive(Default)]
struct Runtime {
    nv:Option<File>,
    prep:Option<Child>,
    suspend:Option<Child>, suspend_generation:Option<u32>,
    netd:Option<Child>,
    rmt:Option<Child>, tftp:Option<Child>, holder:Option<Child>, ipa:Option<Child>, netmgr:Option<Child>,
}
impl Runtime {
    fn netd_registered(timeout:Duration)->Result<bool> {
        let mut output=OpenOptions::new().read(true).write(true).create(true).truncate(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(Path::new(HOME).join("netd-query.result"))?;
        let log=OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(Path::new(HOME).join("netd-query.log"))?;
        let mut command=Command::new("/system/bin/skyward");
        command.arg("venus-netd-check").stderr(log);
        crate::suspend_query::run(command,&mut output,timeout,||END.load(Ordering::Relaxed))
    }

    fn start_netd(&mut self)->Result<()> {
        let until=Instant::now()+Duration::from_secs(15);
        let pid=if let Some(child)=&self.netd {child.id()} else {
            ensure!(!Self::netd_registered(Duration::from_secs(5))?,"refuse to adopt an existing OEM network service");
            let child=spawn("network_netd")?;let pid=child.id();self.netd=Some(child);pid
        };
        let ready=format!("VENUS_NETD_READY pid={pid}");
        loop {
            self.check()?;
            ensure!(!END.load(Ordering::Relaxed),"OEM network readiness cancelled");
            let remaining=until.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(),"OEM network service did not become ready");
            let log=fs::read_to_string(Path::new(HOME).join("network_netd.log"))?;
            if log.lines().any(|line|line==ready) && Self::netd_registered(remaining.min(Duration::from_secs(5)))? {
                self.check()?;return Ok(())
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn stop_netd(&mut self)->Result<()> {
        ensure!(self.netmgr.is_none() && self.ipa.is_none(),"stop OEM network clients before their service");
        let Some(child)=self.netd.as_mut() else {return Ok(())};
        if child.try_wait()?.is_none() {
            ensure!(unsafe{libc::kill(child.id() as i32,libc::SIGTERM)}==0,"signal owned OEM network service");
        }
        let until=Instant::now()+Duration::from_secs(15);
        let status=loop {
            if let Some(status)=child.try_wait()? {break status}
            ensure!(Instant::now()<until,"OEM network cleanup incomplete; retaining service and ownership");
            thread::sleep(Duration::from_millis(100));
        };
        let stopped=format!("VENUS_NETD_STOPPED pid={}",child.id());
        let log=fs::read_to_string(Path::new(HOME).join("network_netd.log"))?;
        ensure!(status.success() && log.lines().any(|line|line==stopped),"OEM network exited without confirmed cleanup; ownership retained");
        self.netd.take();Ok(())
    }

    fn suspend_registered(timeout:Duration)->Result<bool> {
        let mut output=OpenOptions::new().read(true).write(true).create(true).truncate(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(Path::new(HOME).join("suspend-query.result"))?;
        let log=OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(Path::new(HOME).join("suspend-query.log"))?;
        let mut command=Command::new("/system/bin/skyward");
        command.arg("venus-suspend-check").stderr(log);
        crate::suspend_query::run(command,&mut output,timeout,||END.load(Ordering::Relaxed))
            .with_context(||format!("query suspend service; see {HOME}/suspend-query.log"))
    }

    fn netmgr_ready(timeout:Duration)->Result<bool> {
        let mut output=OpenOptions::new().read(true).write(true).create(true).truncate(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(Path::new(HOME).join("netmgr-query.result"))?;
        let log=OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(Path::new(HOME).join("netmgr-query.log"))?;
        let mut command=Command::new("/skyward/runtime/km41-launch");
        command.arg("netmgr_ready").stderr(log);
        // The vendor ping's millisecond argument only limits each poll,
        // not the whole operation. Bound and reap this disposable child.
        crate::suspend_query::run(command,&mut output,timeout,||END.load(Ordering::Relaxed))
    }

    fn start_suspend(&mut self)->Result<()> {
        let until=Instant::now()+Duration::from_secs(15);
        let pid=if let Some(child)=&self.suspend {
            ensure!(self.suspend_generation==Some(child.id()),"suspend child generation changed");
            child.id()
        } else {
            ensure!(self.suspend_generation.is_none(),"old suspend locks require confirmed cleanup");
            ensure!(!Self::suspend_registered(Duration::from_secs(5))?,"refuse to adopt an existing suspend service");
            let child=spawn("network_suspend")?;
            let pid=child.id();self.suspend_generation=Some(pid);self.suspend=Some(child);
            pid
        };
        let ready=format!("VENUS_SUSPEND_READY pid={pid} prefix=skyward-suspend-{pid}-");
        loop {
            self.check()?;
            ensure!(!END.load(Ordering::Relaxed),"suspend service readiness cancelled");
            let remaining=until.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(),"real suspend service did not register; see {HOME}/network_suspend.log");
            let log=fs::read_to_string(Path::new(HOME).join("network_suspend.log"))?;
            // A previous start may have timed out with a live but unready
            // process. Reuse requires the same checks as a new child.
            if log.lines().any(|line|line==ready) && Self::suspend_registered(remaining.min(Duration::from_secs(5)))? {
                self.check()?;
                return Ok(())
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Kernel wakelocks do not belong to an fd. After the exact old
    /// service and its data clients have exited, clear only its tokens.
    fn stop_suspend(&mut self)->Result<()> {
        ensure!(self.netmgr.is_none() && self.ipa.is_none(),"stop suspend clients before their service");
        stop_child(&mut self.suspend,Duration::from_secs(5))?;
        if let Some(pid)=self.suspend_generation {
            let prefix=format!("skyward-suspend-{pid}-");
            for name in fs::read_to_string("/sys/power/wake_lock")?.split_whitespace() {
                if let Some(counter)=name.strip_prefix(&prefix) {
                    ensure!(!counter.is_empty() && counter.bytes().all(|b|b.is_ascii_digit()),"unexpected owned suspend token");
                    fs::write("/sys/power/wake_unlock",name.as_bytes()).context("release exited suspend service's kernel lock")?;
                }
            }
            ensure!(!fs::read_to_string("/sys/power/wake_lock")?.split_whitespace().any(|n|n.starts_with(&prefix)),"owned suspend locks remain active");
            self.suspend_generation=None;
        }
        Ok(())
    }

    /// A crashed service leaves kernel locks behind. Reap its clients before
    /// clearing that generation, including a previous cleanup's partial failure.
    fn cleanup_dead_suspend(&mut self)->Result<()> {
        let dead=match self.suspend.as_mut() {
            Some(child)=>child.try_wait()?.is_some(),
            None=>self.suspend_generation.is_some(),
        };
        if dead {
            stop_child(&mut self.netmgr,Duration::from_secs(8))?;
            stop_child(&mut self.ipa,Duration::from_secs(8))?;
            self.stop_suspend()?;
        }
        Ok(())
    }

    fn prepare(&mut self)->Result<()> {
        let log=OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(Path::new(HOME).join("prepare.log"))?;
        self.prep=Some(Command::new("/skyward/runtime/modem-prepare")
            .stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).spawn()?);
        let until=Instant::now()+Duration::from_secs(60);
        loop {
            if let Some(status)=self.prep.as_mut().unwrap().try_wait()? {
                self.prep.take();
                ensure!(status.success(),"prepare modem runtime failed; see {HOME}/prepare.log");
                return Ok(())
            }
            if END.load(Ordering::Relaxed) || Instant::now()>=until {
                // Let the shell's trap release its own staging mounts. Keep
                // the child and NV lock if cleanup does not finish in time.
                stop_child(&mut self.prep,Duration::from_secs(8))?;
                anyhow::bail!("modem preparation cancelled or timed out; see {HOME}/prepare.log")
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn holder_ready(&self)->Result<bool> {
        let path=Path::new("/skyward/run/state/modem-held.ready");
        let meta=match fs::symlink_metadata(path) {
            Ok(m)=>m,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(false),Err(e)=>return Err(e.into()),
        };
        ensure!(meta.is_file() && meta.uid()==0 && meta.mode()&0o777==0o600,"invalid modem holder receipt");
        let pid=fs::read_to_string(path)?.trim().parse::<u32>()?;
        ensure!(self.holder.as_ref().is_some_and(|c|c.id()==pid),"foreign modem holder receipt");
        Ok(true)
    }

    fn check(&mut self)->Result<()> {
        for (role,child) in [("rmt_storage",&mut self.rmt),("tftp",&mut self.tftp),("holder",&mut self.holder),("network_suspend",&mut self.suspend),("network_netd",&mut self.netd),("ipacm",&mut self.ipa),("netmgr",&mut self.netmgr)] {
            if let Some(child)=child {ensure!(child.try_wait()?.is_none(),"modem helper {role} exited; see {HOME}/{role}.log")}
        }
        Ok(())
    }
    fn start(&mut self)->Result<()> {
        ensure!(self.rmt.is_none() && self.holder.is_none(),"modem runtime already owns helpers");
        self.cleanup_dead_suspend()?;
        self.nv=Some(nv_lock()?);
        crate::venus_runtime::verified()?;
        crate::venus_firmware::publish()?;
        self.prepare()?;
        ensure!(modem_state()?.as_deref()!=Some("ONLINE"),"refuse to adopt an already-online modem");
        self.rmt=Some(spawn("rmt_storage")?);
        self.tftp=Some(spawn("tftp")?);
        thread::sleep(Duration::from_millis(300));self.check()?;
        // The holder's own bounded open runs separately; the UI remains
        // responsive while firmware authentication and RFS requests finish.
        self.holder=Some(spawn("holder")?);
        let until=Instant::now()+Duration::from_secs(50);
        loop {
            self.check()?;
            if self.holder_ready()? && modem_state()?.as_deref()==Some("ONLINE") {break}
            ensure!(!END.load(Ordering::Relaxed) && Instant::now()<until,"modem did not reach ONLINE");
            thread::sleep(Duration::from_millis(100));
        }
        let until=Instant::now()+Duration::from_secs(20);
        loop {
            self.check()?;
            let services=skyward_net::qrtr::discover()?;
            if [1,2,3,11].iter().all(|n| services.iter().any(|e|e.node==0 && e.instance==0 && e.service==*n)){break}
            ensure!(Instant::now()<until,"primary modem QMI services did not appear");
        }
        skyward_net::cellular::radio_online()?;
        self.check()
    }
    fn start_data_helpers(&mut self)->Result<()> {
        self.check()?;
        self.start_suspend()?;
        self.start_netd()?;
        if self.ipa.is_none(){self.ipa=Some(spawn("ipacm")?);thread::sleep(Duration::from_millis(300));}
        if self.netmgr.is_none(){self.netmgr=Some(spawn("netmgr")?);}
        let until=Instant::now()+Duration::from_secs(20);
        let mut last="no READY response".to_string();
        loop {
            self.check()?;
            ensure!(!END.load(Ordering::Relaxed),"netmgr readiness cancelled");
            let remaining=until.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(),"native netmgr did not become ready: {last}; see {HOME}/netmgr-query.log");
            match Self::netmgr_ready(remaining.min(Duration::from_secs(5))) {
                Ok(true) if Path::new("/sys/class/net/rmnet_data0").is_dir()=>{self.check()?;return Ok(())},
                Ok(true)=>last="READY response without the native data channel".into(),
                Ok(false)=>last="native ping did not confirm READY".into(),
                Err(error)=>last=format!("{error:#}"),
            }
            thread::sleep(Duration::from_millis(200));
        }
    }
    fn shutdown(&mut self)->Result<()> {
        stop_child(&mut self.prep,Duration::from_secs(8))?;
        stop_child(&mut self.holder,Duration::from_secs(20))?;
        let until=Instant::now()+Duration::from_secs(25);
        while modem_state()?.as_deref()!=Some("OFFLINE") {
            ensure!(Instant::now()<until,"kernel modem still ONLINE; retaining RMT/TFTP");
            thread::sleep(Duration::from_millis(100));
        }
        ensure!(modem_state()?.as_deref()==Some("OFFLINE"),"kernel modem state is not confirmed OFFLINE; retaining RMT/TFTP");
        stop_child(&mut self.netmgr,Duration::from_secs(8))?;
        stop_child(&mut self.ipa,Duration::from_secs(8))?;
        self.stop_netd()?;
        self.cleanup_dead_suspend()?;
        stop_child(&mut self.tftp,Duration::from_secs(8))?;
        stop_child(&mut self.rmt,Duration::from_secs(8))?;
        self.nv.take();
        Ok(())
    }
}

struct DataRoute {
    expected: iface::Ipv4Configuration,
    settings: Settings,
}

// This owner's stdio can be /dev/null even with init's stdio_to_kmsg.
// Drain the exact owning client's bounded queue explicitly, before a call
// can be dropped. Diagnostics never change the operation's return value.
mod wds_diagnostics {
    use anyhow::Result;
    use skyward_net::{qmi::{CallEndReasons, ProtocolError}, qrtr::WdsDiagnostics};

    // Numeric phase IDs keep all logged fields bounded and free of identities.
    pub(super) const START: u8 = 0;
    pub(super) const SETUP: u8 = 1;
    pub(super) const SETTINGS: u8 = 2;
    pub(super) const CONFIGURED: u8 = 3;
    pub(super) const POLL: u8 = 4;
    pub(super) const STOP: u8 = 5;
    pub(super) const OFFLINE: u8 = 6;

    fn reasons(reason: CallEndReasons) -> String {
        format!("reason_present={} reason={} verbose_present={} verbose_type={} verbose_reason={}",
            u8::from(reason.call_end_reason.is_some()), reason.call_end_reason.unwrap_or(0),
            u8::from(reason.verbose.is_some()), reason.verbose.map_or(0, |r| r.kind),
            reason.verbose.map_or(0, |r| r.reason))
    }

    fn drain(batch: WdsDiagnostics, phase: u8, after: bool, emit: &mut impl FnMut(&str)) {
        let owner = std::process::id();
        let overflow = batch.indications.len().saturating_sub(16);
        for (index, event) in batch.indications.into_iter().take(16).enumerate() {
            emit(&format!("WDS indication: owner={owner} phase={phase} after={} index={index} state={} reconfigure={} family_present={} family={} {}",
                u8::from(after), event.state, u8::from(event.reconfiguration_required),
                u8::from(event.ip_family.is_some()), event.ip_family.unwrap_or(0), reasons(event.reasons)));
        }
        if batch.discarded != 0 || batch.malformed != 0 || overflow != 0 {
            emit(&format!("WDS diagnostic loss: owner={owner} phase={phase} after={} discarded={} malformed={} overflow={overflow}",
                u8::from(after), batch.discarded, batch.malformed));
        }
    }

    pub(super) fn observe<S, T>(session: &mut S, phase: u8,
        take: impl Fn(&mut S) -> WdsDiagnostics,
        operation: impl FnOnce(&mut S) -> Result<T>, mut emit: impl FnMut(&str)) -> Result<T> {
        drain(take(session), phase, false, &mut emit);
        let result = operation(session);
        // Also runs after Err, before callers can stop/drop the failed call.
        drain(take(session), phase, true, &mut emit);
        if let Err(error) = &result {
            let protocol = error.downcast_ref::<ProtocolError>();
            let reason = error.downcast_ref::<CallEndReasons>().copied().unwrap_or_default();
            emit(&format!("WDS operation failed: owner={} phase={phase} protocol_present={} status={} code={} {}",
                std::process::id(), u8::from(protocol.is_some()), protocol.map_or(0, |e| e.status),
                protocol.map_or(0, |e| e.code), reasons(reason)));
        }
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use skyward_net::qmi::{PacketServiceIndication, VerboseCallEndReason};

        fn event(state: u8) -> PacketServiceIndication {
            PacketServiceIndication { state, reconfiguration_required: false, ip_family: Some(4),
                reasons: CallEndReasons { call_end_reason: Some(1), verbose: Some(VerboseCallEndReason { kind: 2, reason: -7 }) } }
        }

        #[test]
        fn drains_before_and_after_once_and_preserves_success() {
            let mut queue = WdsDiagnostics { indications: vec![event(2)], discarded: 0, malformed: 0 };
            let mut lines = Vec::new();
            let result = observe(&mut queue, POLL, std::mem::take, |q| {
                assert!(q.indications.is_empty()); q.indications.push(event(1)); Ok(42)
            }, |line| lines.push(line.to_owned())).unwrap();
            assert_eq!(result, 42);
            assert_eq!(lines.len(), 2);
            assert!(lines[0].contains("after=0 index=0 state=2"));
            assert!(lines[1].contains("after=1 index=0 state=1"));
            observe(&mut queue, STOP, std::mem::take, |_| Ok(()), |line| lines.push(line.to_owned())).unwrap();
            assert_eq!(lines.len(), 2);
        }

        #[test]
        fn failed_start_drains_and_preserves_original_protocol_and_reason() {
            let mut queue = WdsDiagnostics::default();
            let mut lines = Vec::new();
            let reason = event(1).reasons;
            let error = observe::<_, ()>(&mut queue, START, std::mem::take, |q| {
                q.indications.push(event(1));
                Err(anyhow::Error::new(ProtocolError { status: 1, code: 14 }).context(reason).context("start failed"))
            }, |line| lines.push(line.to_owned())).unwrap_err();
            assert!(queue.indications.is_empty());
            assert_eq!(error.downcast_ref::<ProtocolError>().unwrap().code, 14);
            assert_eq!(*error.downcast_ref::<CallEndReasons>().unwrap(), reason);
            assert!(lines[0].contains("state=1"));
            assert!(lines[1].contains("protocol_present=1 status=1 code=14"));
            assert!(lines[1].contains("verbose_present=1 verbose_type=2 verbose_reason=-7"));
        }

        #[test]
        fn counters_are_consumed_once_and_output_is_bounded_numeric() {
            let mut queue = WdsDiagnostics { indications: vec![event(1); 18], discarded: u64::MAX, malformed: 2 };
            let mut lines = Vec::new();
            observe(&mut queue, STOP, std::mem::take, |_| Ok(()), |line| lines.push(line.to_owned())).unwrap();
            assert_eq!(lines.len(), 17);
            assert!(lines[16].contains("malformed=2 overflow=2"));
            assert!(lines.iter().all(|line| line.len() < 512 && !line.contains('\n')));
            observe(&mut queue, OFFLINE, std::mem::take, |_| Ok(()), |line| lines.push(line.to_owned())).unwrap();
            assert_eq!(lines.len(), 17);
        }

        #[test]
        fn transport_error_is_not_invented_as_protocol_or_call_end_reason() {
            let mut queue = WdsDiagnostics::default();
            let mut lines = Vec::new();
            let error = observe::<_, ()>(&mut queue, SETUP, std::mem::take,
                |_| anyhow::bail!("timeout"), |line| lines.push(line.to_owned())).unwrap_err();
            assert_eq!(error.to_string(), "timeout");
            assert!(lines[0].contains("protocol_present=0 status=0 code=0 reason_present=0"));
        }
    }
}

fn wds_operation<T>(session: &mut Session, phase: u8,
    operation: impl FnOnce(&mut Session) -> Result<T>) -> Result<T> {
    wds_diagnostics::observe(session, phase, Session::take_diagnostics, operation,
        |line| { let _ = skyward_sys::kmsg(line); })
}

fn packet_observed(s:&mut State,packet:PacketState,context:&str) {
    s.packet_state=Some(packet);
    let _=skyward_sys::kmsg(&format!("WDS {context}: state={} {packet:?}",packet as u8));
}

fn configure(settings:&Settings, ifindex:u32, configured:&mut Option<DataRoute>)->Result<()> {
    ensure!(fs::read_to_string("/sys/class/net/rmnet_data0/iflink")?.trim()==fs::read_to_string("/sys/class/net/rmnet_ipa0/ifindex")?.trim(),"data interface has a foreign parent");
    let expected=iface::Ipv4Configuration{ifindex,
        address:settings.address,netmask:settings.netmask,mtu:settings.mtu,
        gateway:settings.gateway,metric:100};
    // netmgr owns link/address/MTU. Its KIF transition clears the old address
    // before ip-wrapper adds the WDS address. Never race that transition.
    iface::wait_native_ipv4(IFACE,&expected,Duration::from_secs(20))?;
    iface::add_owned_default(IFACE,&expected)?;
    // Only a successful add creates ownership; pre-existing routes are not
    // adopted, even if their fields happen to match this call.
    *configured=Some(DataRoute{expected,settings:settings.clone()});
    iface::verify_ipv4(IFACE,&configured.as_ref().unwrap().expected)
}

fn withdraw(configured:&mut Option<DataRoute>,s:&mut State)->Result<()> {
    s.address=None;s.dns.clear();
    if let Some(route)=configured.as_ref() {
        let expected=&route.expected;
        if Path::new("/sys/class/net/rmnet_data0").is_dir()
            && iface::read_ipv4(IFACE)?.ifindex==expected.ifindex {
            iface::remove_owned_default(IFACE,expected)?;
        }
    }
    configured.take();Ok(())
}

fn clear_data(call:&mut Option<Session>,configured:&mut Option<DataRoute>,s:&mut State)->Result<()> {
    if let Some(c)=call {wds_operation(c,wds_diagnostics::STOP,Session::stop)?;}
    withdraw(configured,s)?;
    call.take();s.data=Phase::Off;s.packet_state=Some(PacketState::Disconnected);Ok(())
}

fn shutdown_runtime(runtime:&mut Runtime,call:&mut Option<Session>,configured:&mut Option<DataRoute>,s:&mut State)->Result<()> {
    if let Some(c)=call {
        if let Err(e)=wds_operation(c,wds_diagnostics::STOP,Session::stop){let _=skyward_sys::kmsg(&format!("WDS stop unconfirmed; proceeding to owned kernel radio shutdown: {e:#}"));}
    }
    runtime.shutdown()?;
    if let Some(c)=call{wds_operation(c,wds_diagnostics::OFFLINE,Session::release_after_radio_offline)?;}
    clear_data(call,configured,s)
}

pub fn run()->Result<()> {
    ensure!(crate::venus_rw::enabled() && unsafe{libc::geteuid()}==0,"marked Venus Recovery and root required");
    fs::create_dir_all(HOME)?;
    ensure!(fs::symlink_metadata(HOME)?.is_dir(),"modem directory must not be a link");
    fs::set_permissions(HOME,fs::Permissions::from_mode(0o700))?;
    let lock=OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(Path::new(HOME).join("owner.lock"))?;
    ensure!(unsafe{libc::flock(lock.as_raw_fd(),libc::LOCK_EX|libc::LOCK_NB)}==0,"modem already owned");
    unsafe{libc::signal(libc::SIGTERM,finish as *const () as libc::sighandler_t);libc::signal(libc::SIGINT,finish as *const () as libc::sighandler_t);}
    let mut s=State{boot_id:fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim().into(),owner:std::process::id(),..Default::default()};
    let mut runtime=Runtime::default();let mut call=None;let mut configured=None;
    let mut last=String::new();
    let mut pending=Some("radio-on".to_string());
    let mut pending_token=String::new();let mut poll=Instant::now();
    publish(&s);
    loop {
        if END.load(Ordering::Relaxed){pending=Some("radio-off".into());pending_token="shutdown-signal".into();}
        if let Some(command)=pending.take() {
            s.command=pending_token.clone();s.issue=None;
            let result=(||->Result<()> {
                match command.as_str() {
                    "radio-on"=>{
                        if s.radio==Phase::Ready{return Ok(())}
                        s.radio=Phase::Starting;publish(&s);
                        if runtime.nv.is_some(){shutdown_runtime(&mut runtime,&mut call,&mut configured,&mut s)?;}
                        runtime.start()?;s.radio=Phase::Ready;
                    },
                    "radio-off"=>{
                        s.radio=Phase::Stopping;s.data=Phase::Stopping;publish(&s);
                        shutdown_runtime(&mut runtime,&mut call,&mut configured,&mut s)?;
                        if END.load(Ordering::Relaxed){runtime.stop_suspend()?;}
                        s.radio=Phase::Off;
                    },
                    "data-on"=>{
                        ensure!(s.radio==Phase::Ready,"enable the mobile network first");
                        if s.data==Phase::Ready{return Ok(())}
                        if call.is_some(){clear_data(&mut call,&mut configured,&mut s)?;}
                        s.data=Phase::Starting;s.packet_state=None;publish(&s);
                        runtime.start_data_helpers()?;
                        // Wait for the previous native call's asynchronous
                        // teardown before a new generation can reuse its IP.
                        let ifindex=iface::wait_native_idle(IFACE,Duration::from_secs(20))?;
                        call=Some(Session::prepare()?);
                        let c=call.as_mut().unwrap();
                        wds_operation(c,wds_diagnostics::START,Session::start)?;
                        wds_operation(c,wds_diagnostics::SETUP,|c|c.wait_connected(Duration::from_secs(15),|packet|{packet_observed(&mut s,packet,"setup");publish(&s);}))?;
                        let settings=wds_operation(c,wds_diagnostics::SETTINGS,Session::settings)?;
                        configure(&settings,ifindex,&mut configured)?;
                        wds_operation(c,wds_diagnostics::CONFIGURED,|c|c.wait_connected(Duration::from_secs(15),|packet|{packet_observed(&mut s,packet,"configured");publish(&s);}))?;
                        s.active_packet(PacketState::Connected,&settings)?;
                    },
                    "data-off"=>{s.data=Phase::Stopping;publish(&s);clear_data(&mut call,&mut configured,&mut s)?;},
                    _=>anyhow::bail!("unknown modem command"),
                }
                Ok(())
            })();
            if let Err(e)=result {
                if command.starts_with("radio-"){s.radio=Phase::Failed}else{s.data=Phase::Failed}
                s.issue=Some(format!("{e:#}"));
                if command=="radio-on" && runtime.prep.is_none() && runtime.rmt.is_none() && runtime.tftp.is_none() && runtime.holder.is_none(){runtime.nv.take();}
                // Withdraw only our default route, retaining the WDS handle
                // for explicit verified stop. Native netmgr owns IP/link.
                if command=="data-on" {
                    s.packet_state=None;
                    if let Err(cleanup)=withdraw(&mut configured,&mut s){s.issue=Some(format!("{e:#}; withdraw owned address: {cleanup:#}"));}
                }
            }
            publish(&s);
            if END.load(Ordering::Relaxed) && s.radio==Phase::Off{break}
            // On a failed shutdown keep this init group (and NV helpers)
            // alive. A visible failure is preferable to killing NV writes.
            // A signal received during another bounded operation still
            // needs its radio-off transition on the next iteration.
            if command=="radio-off" {END.store(false,Ordering::Relaxed);}
        }
        let asked=skyward_sys::prop(COMMAND).unwrap_or_default();
        if asked!=last {
            last=asked.clone();
            // Properties survive a service restart, but a command belongs
            // to exactly the init owner that acknowledged its generation.
            if asked.starts_with(&format!("{}/",s.owner)) {
                if let Some((_,cmd))=asked.split_once(':'){pending_token=asked.clone();pending=Some(cmd.to_string());}
            }
        }
        if poll.elapsed()>=Duration::from_secs(5) && s.radio==Phase::Ready {
            poll=Instant::now();
            if let Err(e)=runtime.check(){
                s.radio=Phase::Failed;s.data=Phase::Failed;s.issue=Some(format!("{e:#}"));s.dns.clear();s.address=None;
                if let Err(cleanup)=withdraw(&mut configured,&mut s){s.issue=Some(format!("{e:#}; withdraw owned address: {cleanup:#}"));}
                publish(&s);
                // Keep the WDS owner and NV helpers until explicit stop or
                // confirmed OFFLINE, but do not strand a dead bridge's locks.
                if let Err(cleanup)=runtime.cleanup_dead_suspend(){s.issue=Some(format!("{e:#}; clean exited suspend service: {cleanup:#}"));}
            }
            // Keep observing a retained call even after a query failure.
            // Suspension/authentication is not permission to send Stop.
            if s.radio==Phase::Ready && call.is_some() {
                match wds_operation(call.as_mut().unwrap(),wds_diagnostics::POLL,Session::state) {
                    Ok(PacketState::Disconnected)=>{
                        packet_observed(&mut s,PacketState::Disconnected,"poll");
                        s.address=None;s.dns.clear();
                        if let Err(e)=clear_data(&mut call,&mut configured,&mut s){s.data=Phase::Failed;s.issue=Some(format!("{e:#}"));}
                        else{s.issue=None;}
                    },
                    Ok(packet)=>{
                        packet_observed(&mut s,packet,"poll");
                        let verified=(||->Result<()> {
                            let route=configured.as_ref().context("no verified owned route; explicit retry required")?;
                            iface::verify_ipv4(IFACE,&route.expected)?;
                            s.active_packet(packet,&route.settings)
                        })();
                        if let Err(e)=verified {
                            s.data=Phase::Failed;s.address=None;s.dns.clear();
                            s.issue=Some(format!("mobile interface configuration changed: {e:#}"));
                            if let Err(cleanup)=withdraw(&mut configured,&mut s){s.issue=Some(format!("{e:#}; withdraw owned route: {cleanup:#}"));}
                        }
                    },
                    Err(e)=>{
                        s.packet_state=None;s.data=Phase::Failed;s.address=None;s.dns.clear();
                        s.issue=Some(format!("WDS state unknown; call and route retained: {e:#}"));
                        let _=skyward_sys::kmsg(&format!("WDS poll: state=unknown; call and route retained: {e:#}"));
                    },
                }
                publish(&s);
            }
        }
        publish(&s);
        thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}
