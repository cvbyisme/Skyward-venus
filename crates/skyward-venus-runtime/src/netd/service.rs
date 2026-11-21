#[cfg(target_os = "android")]
use super::backend::LinuxBackend;
use super::manager::{Backend, Manager};
use anyhow::Result;
#[cfg(target_os = "android")]
use anyhow::ensure;
#[cfg(target_os = "android")]
use skyward_binder::{driver::Binder, hidl};
use skyward_binder::{
    hidl::Message,
    hidl_serve::{self, Interface},
    raw::{Call, Reply, Service},
};
use std::sync::Mutex;
#[cfg(target_os = "android")]
use std::{
    io::{self, Write},
    sync::Arc,
};
const NETD: &str = "android.system.net.netd@1.1::INetd";
const NETD10: &str = "android.system.net.netd@1.0::INetd";
const CHAIN: [Interface; 3] = [
    Interface {
        name: NETD,
        hash: "421aac09f4928daebbbffe3302f15c6f68d2b5e4b9c671e528da8a599133906c",
    },
    Interface {
        name: NETD10,
        hash: "2cfd7918acf09462fe8fe3f5608be2956c9f737c78d3bb3f176cc5531c09f5d6",
    },
    hidl_serve::BASE_INTERFACE,
];
struct Endpoint<B>(Mutex<Manager<B>>);
fn status(error: i32, method: u32) -> u32 {
    if error != 0 {
        eprintln!("VENUS_NETD_ERROR method={method} errno={}", -error);
    }
    match -error {
        0 => 0,
        libc::EINVAL => 1,
        libc::ENONET => 2,
        libc::EEXIST => 3,
        libc::EPERM | libc::EACCES => 4,
        _ => 5,
    }
}
impl<B: Backend> Endpoint<B> {
    fn answer(
        &self,
        object: u64,
        code: u32,
        one_way: bool,
        uid: u32,
        read: &mut skyward_binder::hidl::Answer<'_>,
    ) -> Result<Reply> {
        if object != 1 {
            return Ok(Reply::Status(-74));
        }
        if code >= 256_000_000 {
            return hidl_serve::base(code, one_way, read, &CHAIN);
        }
        if one_way {
            return Ok(Reply::Status(-22));
        }
        let interface = read.interface()?;
        // Inherited methods keep the declaring 1.0 interface token.
        if interface != if code <= 2 { NETD10 } else { NETD } {
            return Ok(Reply::Status(-74));
        }
        let allowed = uid == 0 || uid == 1001;
        let mut reply = Message::ok();
        let mut manager = self.0.lock().unwrap();
        let result = match code {
            1 => {
                read.finish()?;
                let result = if allowed {
                    manager.create()
                } else {
                    Err(-libc::EPERM)
                };
                let (handle, mark) = result.unwrap_or((0, 0));
                reply.u64(handle);
                reply.u32(mark);
                result.map(|_| ())
            }
            2 => {
                let handle = read.u64()?;
                read.finish()?;
                if allowed {
                    manager.destroy(handle)
                } else {
                    Err(-libc::EPERM)
                }
            }
            5 | 6 => {
                let handle = read.u64()?;
                let name = read.string(16 * 1024)?;
                read.finish()?;
                if !allowed {
                    Err(-libc::EPERM)
                } else if code == 5 {
                    manager.add_interface(handle, name)
                } else {
                    manager.remove_interface(handle, name)
                }
            }
            3 | 4 => {
                let handle = read.u64()?;
                let name = read.string(16 * 1024)?;
                let dest = read.string(16 * 1024)?;
                let gateway = read.string(16 * 1024)?;
                read.finish()?;
                if allowed {
                    manager.route(handle, name, dest, gateway, code == 3)
                } else {
                    Err(-libc::EPERM)
                }
            }
            7 => {
                read.bool()?;
                read.finish()?;
                Err(if allowed {
                    -libc::EOPNOTSUPP
                } else {
                    -libc::EPERM
                })
            }
            8 => {
                read.string(16 * 1024)?;
                read.string(16 * 1024)?;
                read.bool()?;
                read.finish()?;
                Err(if allowed {
                    -libc::EOPNOTSUPP
                } else {
                    -libc::EPERM
                })
            }
            _ => return Ok(Reply::Status(-74)),
        };
        reply.u32(status(result.err().unwrap_or(0), code));
        Ok(Reply::Hidl(reply))
    }
}
impl<C, B: Backend + Send> Service<C> for Endpoint<B> {
    fn call(&self, _: &C, call: Call<'_>) -> Reply {
        self.answer(
            call.object,
            call.code,
            call.one_way(),
            call.uid,
            &mut call.hidl(),
        )
        .unwrap_or(Reply::Status(-libc::EINVAL))
    }
}
#[cfg(target_os = "android")]
pub fn run() -> Result<()> {
    ensure!(unsafe { libc::geteuid() } == 0, "root required");
    // Block before creating workers; only the main sigwait owner cleans up.
    let mut signals: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut signals);
        libc::sigaddset(&mut signals, libc::SIGTERM);
        libc::sigaddset(&mut signals, libc::SIGINT);
    }
    ensure!(
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut()) } == 0,
        "block shutdown signals"
    );
    let _lock = crate::service_lock::lock("network-netd")?;
    let endpoint = Arc::new(Endpoint(Mutex::new(Manager::new(
        LinuxBackend::default(),
        format!("skyward-oem-{}", std::process::id()),
    ))));
    let binder = Arc::new(Binder::raw_serving(
        std::path::Path::new("/dev/hwbinder"),
        endpoint.clone(),
    )?);
    for interface in [NETD, NETD10] {
        ensure!(
            hidl::find(&binder, interface, "default")?.is_none(),
            "INetd/default already has an owner"
        );
    }
    hidl_serve::register(&binder, "default", 1)?;
    for _ in 0..2 {
        let binder = binder.clone();
        std::thread::spawn(move || match binder.serve() {
            Err(e) => {
                eprintln!("VENUS_NETD_FATAL binder={e:#}");
                unsafe { libc::_exit(7) }
            }
            Ok(never) => match never {},
        });
    }
    println!("VENUS_NETD_READY pid={}", std::process::id());
    io::stdout().flush()?;
    loop {
        let mut signal = 0;
        if unsafe { libc::sigwait(&signals, &mut signal) } != 0 {
            continue;
        }
        match endpoint.0.lock().unwrap().shutdown() {
            Err(error) => eprintln!(
                "VENUS_NETD_STOP_FAILED pid={} errno={}",
                std::process::id(),
                -error
            ),
            Ok(()) => {
                println!("VENUS_NETD_STOPPED pid={}", std::process::id());
                io::stdout().flush()?;
                unsafe { libc::_exit(0) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::Fake;
    use super::*;
    use skyward_binder::{hidl::Answer, parcel::Payload};
    fn answer(endpoint: &Endpoint<Fake>, uid: u32, code: u32, request: &Message) -> Message {
        match endpoint
            .answer(1, code, false, uid, &mut Answer::new(request.values()))
            .unwrap()
        {
            Reply::Hidl(reply) => reply,
            _ => panic!("expected method reply"),
        }
    }
    fn result(reply: &Message) -> i32 {
        let mut read = Answer::new(reply.values());
        read.status().unwrap();
        let code = read.i32().unwrap();
        read.finish().unwrap();
        code
    }
    #[test]
    fn android_11_transaction_order_and_declaring_interfaces() {
        // Official INetd@1.0 has only create/destroy; 1.1 declares route
        // add/remove before interface add/remove, independently of C++ order.
        let endpoint = Endpoint(Mutex::new(Manager::new(Fake::new(), "owner".into())));
        let create = Message::call(NETD10);
        let reply = answer(&endpoint, 1001, 1, &create);
        let mut read = Answer::new(reply.values());
        read.status().unwrap();
        let handle = read.u64().unwrap();
        assert_eq!(read.i32().unwrap(), 0xf0001);
        assert_eq!(read.i32().unwrap(), 0);
        read.finish().unwrap();
        let mut iface = Message::call(NETD);
        iface.u64(handle);
        iface.text("rmnet_data0");
        assert_eq!(result(&answer(&endpoint, 1001, 5, &iface)), 0);
        let mut route = Message::call(NETD);
        route.u64(handle);
        route.text("rmnet_data0");
        route.text("192.0.2.0/24");
        route.text("");
        assert_eq!(result(&answer(&endpoint, 1001, 3, &route)), 0);
        assert_eq!(result(&answer(&endpoint, 0, 4, &route)), 0);
        assert_eq!(result(&answer(&endpoint, 1001, 6, &iface)), 0);
        let mut destroy = Message::call(NETD10);
        destroy.u64(handle);
        assert_eq!(result(&answer(&endpoint, 0, 2, &destroy)), 0);
        let wrong = Message::call(NETD);
        assert!(matches!(
            endpoint
                .answer(1, 1, false, 0, &mut Answer::new(wrong.values()))
                .unwrap(),
            Reply::Status(-74)
        ));
        let base = Message::call(hidl_serve::BASE);
        assert!(matches!(
            endpoint
                .answer(1, 256_067_662, false, 0, &mut Answer::new(base.values()))
                .unwrap(),
            Reply::Hidl(_)
        ));
    }
    #[test]
    fn caller_uid_cannot_mutate_networks_and_forwarding_is_not_faked() {
        let endpoint = Endpoint(Mutex::new(Manager::new(Fake::new(), "owner".into())));
        let create = Message::call(NETD10);
        let denied = answer(&endpoint, 2000, 1, &create);
        let mut read = Answer::new(denied.values());
        read.status().unwrap();
        assert_eq!(read.u64().unwrap(), 0);
        assert_eq!(read.i32().unwrap(), 0);
        assert_eq!(read.i32().unwrap(), 4);
        let created = answer(&endpoint, 0, 1, &create);
        let mut read = Answer::new(created.values());
        read.status().unwrap();
        let handle = read.u64().unwrap();
        let mut iface = Message::call(NETD);
        iface.u64(handle);
        iface.text("rmnet_data0");
        assert_eq!(result(&answer(&endpoint, 2000, 5, &iface)), 4);
        let mut forward = Message::call(NETD);
        forward.u32(1);
        assert_eq!(result(&answer(&endpoint, 0, 7, &forward)), 5);
        assert_eq!(result(&answer(&endpoint, 2000, 7, &forward)), 4);
        let mut destroy = Message::call(NETD10);
        destroy.u64(handle);
        assert_eq!(result(&answer(&endpoint, 2000, 2, &destroy)), 4);
        assert_eq!(result(&answer(&endpoint, 0, 2, &destroy)), 0);
        assert!(matches!(
            endpoint
                .answer(1, 1, true, 0, &mut Answer::new(create.values()))
                .unwrap(),
            Reply::Status(-22)
        ));
    }
}
