//! Android SystemSuspend 1.0 backed by real kernel wake_lock/wake_unlock writes.
use anyhow::{Context, Result, ensure};
use skyward_binder::{
    hidl::Message,
    hidl_serve::{self, Interface},
    raw::{Call, Reply, Service},
};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    sync::{Arc, Mutex},
};

#[cfg(target_os = "android")]
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
};

pub(super) const SUSPEND: &str = "android.system.suspend@1.0::ISystemSuspend";
pub(super) const LOCK: &str = "android.system.suspend@1.0::IWakeLock";
const SUSPEND_CHAIN: [Interface; 2] = [
    Interface {
        name: SUSPEND,
        hash: "33917c1ec8ce300d5f50cf4b00ced1333d77d11d8b8314a70fc7adab008553fc",
    },
    hidl_serve::BASE_INTERFACE,
];
const LOCK_CHAIN: [Interface; 2] = [
    Interface {
        name: LOCK,
        hash: "4a288d439addc1a1c58d55495f4f1e5602cbb0d12e29f39b985151e165d016ac",
    },
    hidl_serve::BASE_INTERFACE,
];

trait Operations: Send + Sync {
    fn acquire(&self, token: &str) -> Result<()>;
    fn release(&self, token: &str) -> Result<()>;
}
#[cfg(target_os = "android")]
struct Sysfs(Mutex<(File, File)>);
#[cfg(target_os = "android")]
impl Sysfs {
    fn open() -> Result<Self> {
        let open = |path| {
            OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)
                .with_context(|| format!("open {path}"))
        };
        Ok(Self(Mutex::new((
            open("/sys/power/wake_lock")?,
            open("/sys/power/wake_unlock")?,
        ))))
    }
}
fn write_token(file: &mut impl Write, token: &str) -> Result<()> {
    // A partial sysfs write is an error; the suffix must not become a new token.
    let count = loop {
        match file.write(token.as_bytes()) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => break result?,
        }
    };
    ensure!(count == token.len(), "short kernel wake lock write");
    Ok(())
}
#[cfg(target_os = "android")]
impl Operations for Sysfs {
    fn acquire(&self, token: &str) -> Result<()> {
        write_token(&mut self.0.lock().unwrap().0, token)
    }
    fn release(&self, token: &str) -> Result<()> {
        write_token(&mut self.0.lock().unwrap().1, token)
    }
}
struct Tokens {
    prefix: String,
    slots: Mutex<(u64, Vec<u64>)>,
}
impl Tokens {
    fn new(pid: u32) -> Result<Self> {
        ensure!(pid > 0, "invalid suspend owner PID");
        Ok(Self {
            prefix: format!("skyward-suspend-{pid}-"),
            slots: Mutex::new((1, Vec::new())),
        })
    }
    fn take(&self) -> Result<(u64, String)> {
        let mut slots = self.slots.lock().unwrap();
        let id = if let Some(id) = slots.1.pop() {
            id
        } else {
            let id = slots.0;
            slots.0 = id
                .checked_add(1)
                .context("suspend token counter exhausted")?;
            id
        };
        Ok((id, format!("{}{id}", self.prefix)))
    }
    fn give(&self, id: u64) {
        self.slots.lock().unwrap().1.push(id);
    }
}
struct Lease {
    backend: Arc<dyn Operations>,
    tokens: Arc<Tokens>,
    id: u64,
    name: String,
    active: Mutex<bool>,
}
impl Lease {
    fn acquire(backend: Arc<dyn Operations>, tokens: Arc<Tokens>) -> Result<Self> {
        let (id, name) = tokens.take()?;
        // On failure do not recycle the slot: a lost write result can leave a
        // live kernel source, and the endpoint must die for owner cleanup.
        backend.acquire(&name)?;
        Ok(Self {
            backend,
            tokens,
            id,
            name,
            active: Mutex::new(true),
        })
    }
    fn release(&self) -> Result<()> {
        let mut active = self.active.lock().unwrap();
        if *active {
            self.backend.release(&self.name)?;
            *active = false;
            self.tokens.give(self.id);
        }
        Ok(())
    }
}
fn fatal(phase: &str, error: impl std::fmt::Display) -> ! {
    eprintln!(
        "VENUS_SUSPEND_FATAL pid={} phase={phase} error={error}",
        std::process::id()
    );
    // Legacy libpower handles DEAD_OBJECT. Returning success/null or an ordinary
    // HIDL exception after kernel failure would give a false wake-lock receipt.
    unsafe { libc::_exit(74) }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Err(error) = self.release() {
            fatal("destructor-release", error);
        }
    }
}
struct Endpoint {
    backend: Arc<dyn Operations>,
    tokens: Arc<Tokens>,
    objects: Mutex<(u64, BTreeMap<u64, Arc<Lease>>)>,
}
impl Endpoint {
    fn answer(
        &self,
        object: u64,
        code: u32,
        one_way: bool,
        read: &mut skyward_binder::hidl::Answer<'_>,
    ) -> Result<Reply> {
        let lease = self.objects.lock().unwrap().1.get(&object).cloned();
        if object != 1 && lease.is_none() {
            return Ok(Reply::Status(-74));
        }
        if code >= 256_000_000 {
            return hidl_serve::base(
                code,
                one_way,
                read,
                if object == 1 {
                    &SUSPEND_CHAIN
                } else {
                    &LOCK_CHAIN
                },
            );
        }
        if one_way != (object != 1) {
            return Ok(Reply::Status(-22));
        }
        if read.interface()? != if object == 1 { SUSPEND } else { LOCK } || code != 1 {
            return Ok(Reply::Status(-74));
        }
        let mut reply = Message::ok();
        if let Some(lease) = lease {
            read.finish()?;
            if let Err(error) = lease.release() {
                fatal("release", error);
            }
        } else {
            let kind = read.i32()?;
            read.string(16 * 1024)?; // debug name is never used as a kernel token
            read.finish()?;
            ensure!(kind == 0 || kind == 1, "unsupported wake lock type");
            let lease = Arc::new(
                Lease::acquire(self.backend.clone(), self.tokens.clone())
                    .unwrap_or_else(|e| fatal("acquire", e)),
            );
            let mut objects = self.objects.lock().unwrap();
            let number = objects.0;
            objects.0 = number
                .checked_add(1)
                .unwrap_or_else(|| fatal("object", "counter exhausted"));
            objects.1.insert(number, lease);
            reply.local(number);
        }
        Ok(Reply::Hidl(reply))
    }
}
impl<C> Service<C> for Endpoint {
    fn call(&self, _: &C, call: Call<'_>) -> Reply {
        self.answer(call.object, call.code, call.one_way(), &mut call.hidl())
            .unwrap_or_else(|e| Reply::Hidl(Message::exception(-3, &e.to_string())))
    }
    fn release(&self, _: &C, object: u64) {
        let lease = self.objects.lock().unwrap().1.remove(&object);
        drop(lease); // release outside registry lock, and only after last strong ref
    }
}

#[cfg(target_os = "android")]
pub fn run() -> Result<()> {
    use skyward_binder::{driver::Binder, hidl};
    ensure!(unsafe { libc::geteuid() } == 0, "root required");
    let _lock = crate::service_lock::lock("network-suspend")?;
    let endpoint = Arc::new(Endpoint {
        backend: Arc::new(Sysfs::open()?),
        tokens: Arc::new(Tokens::new(std::process::id())?),
        objects: Mutex::new((2, BTreeMap::new())),
    });
    let binder = Arc::new(Binder::raw_serving(
        std::path::Path::new("/dev/hwbinder"),
        endpoint.clone(),
    )?);
    ensure!(
        hidl::find(&binder, SUSPEND, "default")?.is_none(),
        "SystemSuspend/default already has an owner"
    );
    hidl_serve::register(&binder, "default", 1)?;
    println!(
        "VENUS_SUSPEND_READY pid={} prefix={}",
        std::process::id(),
        endpoint.tokens.prefix
    );
    io::stdout().flush()?;
    let worker = binder.clone();
    std::thread::spawn(move || match worker.serve() {
        Err(e) => fatal("binder-worker", e),
        Ok(never) => match never {},
    });
    match binder.serve() {
        Err(e) => fatal("binder", e),
        Ok(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    #[derive(Default)]
    struct Model(Mutex<State>);
    #[derive(Default)]
    struct State {
        held: BTreeSet<String>,
        ever: BTreeSet<String>,
        writes: usize,
        fail_acquire: bool,
        fail_release: bool,
    }
    impl Operations for Model {
        fn acquire(&self, token: &str) -> Result<()> {
            let mut s = self.0.lock().unwrap();
            ensure!(!s.fail_acquire, "injected acquire error");
            assert!(s.held.insert(token.into()));
            s.ever.insert(token.into());
            s.writes += 1;
            Ok(())
        }
        fn release(&self, token: &str) -> Result<()> {
            let mut s = self.0.lock().unwrap();
            ensure!(!s.fail_release, "injected release error");
            assert!(s.held.remove(token));
            s.writes += 1;
            Ok(())
        }
    }
    #[test]
    fn hardware_object_release_is_oneway_and_reference_drop_releases_kernel_lease() {
        use skyward_binder::{hidl::Answer, parcel::Payload};
        let backend = Arc::new(Model::default());
        let endpoint = Endpoint {
            backend: backend.clone(),
            tokens: Arc::new(Tokens::new(1234).unwrap()),
            objects: Mutex::new((2, BTreeMap::new())),
        };
        let mut request = Message::call(SUSPEND);
        request.u32(0);
        request.text("equal debug names");
        let reply = endpoint
            .answer(1, 1, false, &mut Answer::new(request.values()))
            .unwrap();
        let Reply::Hidl(reply) = reply else {
            panic!("expected object reply")
        };
        assert_eq!(reply.values().len(), 28); // status + native flat object, no AIDL word
        assert_eq!(backend.0.lock().unwrap().held.len(), 1);
        let release = Message::call(LOCK);
        assert!(matches!(
            endpoint
                .answer(2, 1, false, &mut Answer::new(release.values()))
                .unwrap(),
            Reply::Status(-22)
        ));
        endpoint
            .answer(2, 1, true, &mut Answer::new(release.values()))
            .unwrap();
        endpoint
            .answer(2, 1, true, &mut Answer::new(release.values()))
            .unwrap();
        assert!(backend.0.lock().unwrap().held.is_empty());
        endpoint
            .answer(1, 1, false, &mut Answer::new(request.values()))
            .unwrap();
        <Endpoint as Service<()>>::release(&endpoint, &(), 3);
        assert!(backend.0.lock().unwrap().held.is_empty());
        let base = Message::call(hidl_serve::BASE);
        assert!(matches!(
            endpoint
                .answer(1, 256_067_662, false, &mut Answer::new(base.values()))
                .unwrap(),
            Reply::Hidl(_)
        ));
        assert!(matches!(
            endpoint
                .answer(3, 1, true, &mut Answer::new(release.values()))
                .unwrap(),
            Reply::Status(-74)
        ));
        let mut bad = Message::call(SUSPEND);
        bad.u32(2);
        bad.text("invalid");
        assert!(
            endpoint
                .answer(1, 1, false, &mut Answer::new(bad.values()))
                .is_err()
        );
        assert!(backend.0.lock().unwrap().held.is_empty());
    }
    #[test]
    fn independent_leases_reuse_slots_without_releasing_replacements() {
        let m = Arc::new(Model::default());
        let tokens = Arc::new(Tokens::new(1234).unwrap());
        let first = Lease::acquire(m.clone(), tokens.clone()).unwrap();
        let second = Lease::acquire(m.clone(), tokens.clone()).unwrap();
        first.release().unwrap();
        first.release().unwrap();
        let replacement = Lease::acquire(m.clone(), tokens.clone()).unwrap();
        assert_eq!(replacement.name, first.name);
        first.release().unwrap();
        drop(first);
        assert_eq!(m.0.lock().unwrap().held.len(), 2);
        drop((second, replacement));
        for _ in 0..10_000 {
            drop(Lease::acquire(m.clone(), tokens.clone()).unwrap());
        }
        assert_eq!(m.0.lock().unwrap().ever.len(), 2);
        assert!(m.0.lock().unwrap().held.is_empty());
    }
    #[test]
    fn failure_retains_ownership_and_does_not_recycle_active_slot() {
        let m = Arc::new(Model::default());
        let tokens = Arc::new(Tokens::new(1234).unwrap());
        m.0.lock().unwrap().fail_acquire = true;
        assert!(Lease::acquire(m.clone(), tokens.clone()).is_err());
        m.0.lock().unwrap().fail_acquire = false;
        let held = Lease::acquire(m.clone(), tokens.clone()).unwrap();
        m.0.lock().unwrap().fail_release = true;
        assert!(held.release().is_err());
        let next = Lease::acquire(m.clone(), tokens).unwrap();
        assert_ne!(held.name, next.name);
        assert_eq!(m.0.lock().unwrap().held.len(), 2);
        m.0.lock().unwrap().fail_release = false;
        drop((held, next));
        assert!(m.0.lock().unwrap().held.is_empty());
    }
    #[test]
    fn concurrent_release_is_idempotent_and_names_are_bounded() {
        let m = Arc::new(Model::default());
        let tokens = Arc::new(Tokens::new(1234).unwrap());
        let shared = Arc::new(Lease::acquire(m.clone(), tokens.clone()).unwrap());
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| shared.release().unwrap());
            }
        });
        assert_eq!(m.0.lock().unwrap().writes, 2);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    for _ in 0..1000 {
                        drop(Lease::acquire(m.clone(), tokens.clone()).unwrap());
                    }
                });
            }
        });
        let state = m.0.lock().unwrap();
        assert!(state.held.is_empty());
        assert!(state.ever.len() <= 16);
    }
    #[test]
    fn short_sysfs_write_is_never_retried_as_a_suffix() {
        struct Short(usize);
        impl Write for Short {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0 += 1;
                Ok(bytes.len() - 1)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut short = Short(0);
        assert!(write_token(&mut short, "skyward-suspend-1-1").is_err());
        assert_eq!(short.0, 1);
    }
}
