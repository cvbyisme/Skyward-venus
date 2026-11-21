use super::{
    backend::default_routes,
    manager::{mark, netid},
};
use anyhow::{Context, Result, ensure};
use skyward_binder::{
    driver::{Binder, Handle},
    hidl::{self, Message},
    hidl_serve::BASE,
};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    io::{self, Write},
};
const NETD10: &str = "android.system.net.netd@1.0::INetd";
fn kernel<T>(result: super::manager::Result<T>) -> Result<T> {
    result.map_err(|e| std::io::Error::from_raw_os_error(-e).into())
}
fn unchanged(before: &BTreeSet<String>, phase: &str) -> Result<()> {
    let now = kernel(default_routes())?;
    if now != *before {
        for row in before.difference(&now) {
            eprintln!("REMOVED {row}");
        }
        for row in now.difference(before) {
            eprintln!("ADDED {row}");
        }
        anyhow::bail!(
            "{phase}: default route set changed; concurrent changes also fail this check"
        );
    }
    println!(
        "PASS phase={phase} default_routes={} unchanged=true",
        now.len()
    );
    io::stdout().flush()?;
    Ok(())
}
fn destroy(service: &Handle<'_>, handle: u64) -> Result<i32> {
    let mut call = Message::call(NETD10);
    call.u64(handle);
    let reply = service.transact(2, &call)?;
    let mut read = reply.hidl();
    read.status()?;
    let status = read.i32()?;
    read.finish()?;
    Ok(status)
}
struct Network<'a, 'b> {
    service: &'b Handle<'a>,
    handle: u64,
    held: bool,
}
impl<'a, 'b> Network<'a, 'b> {
    fn create(service: &'b Handle<'a>) -> Result<Self> {
        let reply = service.transact(1, &Message::call(NETD10))?;
        let mut read = reply.hidl();
        read.status()?;
        let handle = read.u64()?;
        let fwmark = read.i32()? as u32;
        let status = read.i32()?;
        let network = Self {
            service,
            handle,
            held: status == 0,
        };
        read.finish()?;
        ensure!(status == 0, "create failed with status {status}");
        let id = kernel(netid(handle))?;
        ensure!(fwmark == mark(id), "incorrect AOSP SYSTEM network mark");
        println!(
            "CREATED handle=0x{handle:x} mark=0x{fwmark:x} pid={}",
            std::process::id()
        );
        io::stdout().flush()?;
        Ok(network)
    }
    fn destroy(&mut self) -> Result<()> {
        ensure!(destroy(self.service, self.handle)? == 0, "destroy failed");
        self.held = false;
        Ok(())
    }
}
impl Drop for Network<'_, '_> {
    fn drop(&mut self) {
        if self.held {
            match destroy(self.service, self.handle) {
                Ok(0) => {}
                other => eprintln!("CLEANUP_FAILED handle=0x{:x} result={other:?}", self.handle),
            }
        }
    }
}
pub fn run(args: &[OsString]) -> Result<()> {
    ensure!(
        args.is_empty() || (args.len() == 1 && args[0] == "--lifecycle"),
        "usage: network-netd-probe [--lifecycle]"
    );
    unsafe {
        libc::alarm(20);
    }
    let binder = Binder::hardware()?;
    let service = hidl::find(&binder, "android.system.net.netd@1.1::INetd", "default")?
        .context("HIDL Netd 1.1/default is not registered")?;
    service
        .transact(256_921_159, &Message::call(BASE))?
        .hidl()
        .status()?;
    println!("PASS HIDL android.system.net.netd@1.1::INetd/default ping");
    io::stdout().flush()?;
    if !args.is_empty() {
        let before = kernel(default_routes())?;
        let mut first = Network::create(&service)?;
        let mut second = Network::create(&service)?;
        ensure!(
            first.handle != second.handle,
            "independent creates returned the same live network"
        );
        unchanged(&before, "two-empty-networks-created")?;
        for bad in [first.handle ^ 1, 0] {
            ensure!(
                destroy(&service, bad)? == 1,
                "malformed handle was not rejected"
            );
        }
        unchanged(&before, "invalid-handles-rejected")?;
        first.destroy()?;
        ensure!(
            destroy(&service, first.handle)? == 2,
            "destroyed network still accepted"
        );
        second.destroy()?;
        unchanged(&before, "both-networks-destroyed")?;
    }
    unsafe {
        libc::alarm(0);
    }
    Ok(())
}
