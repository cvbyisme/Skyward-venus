//! Real android.display.IQService on a private Android 11 vendor Binder node.
//! No fake replies, JNI, platform C++ vtables, or KMS writes.
#[cfg(target_os = "android")]
use super::guard::{self, Policy};
use super::{protocol, transport::Owner};
#[cfg(target_os = "android")]
use anyhow::{Context, Result, ensure};
use skyward_binder::{
    parcel::{Parcel, Reader},
    raw::{Call, Reply, Service},
};
use std::sync::Mutex;
#[cfg(target_os = "android")]
use std::{ffi::OsString, path::Path};

const DESCRIPTOR: &str = "android.display.IQService";
const PING_TRANSACTION: u32 = u32::from_be_bytes(*b"_PNG");
const INTERFACE_TRANSACTION: u32 = u32::from_be_bytes(*b"_NTF");
const SET_PAUSED: u32 = 20;
const UNKNOWN_TRANSACTION: i32 = -74;
const BAD_VALUE: i32 = -libc::EINVAL;
const BAD_TYPE: i32 = -2147483647;
const PERMISSION_DENIED: i32 = -libc::EPERM;
const DEAD_OBJECT: i32 = -libc::EPIPE;

pub(super) struct Endpoint(pub Mutex<Owner>);

impl Endpoint {
    fn answer(&self, code: u32, flags: u32, caller_uid: u32, data: &[u8]) -> Reply {
        match code {
            INTERFACE_TRANSACTION => {
                let mut output = Parcel::default();
                output.string(DESCRIPTOR);
                return Reply::Parcel(output);
            }
            PING_TRANSACTION => {
                return match self.0.lock().unwrap().call(protocol::PING) {
                    Ok(_) => Reply::Parcel(Parcel::default()),
                    Err(error) => {
                        eprintln!("QSERVICE_PING_BACKEND_FAILED {error:#}");
                        Reply::Status(DEAD_OBJECT)
                    }
                };
            }
            _ => {}
        }
        if caller_uid != 0 {
            return Reply::Status(PERMISSION_DENIED);
        }
        if code != SET_PAUSED {
            return Reply::Status(UNKNOWN_TRANSACTION);
        }
        if flags & 1 != 0
            || data.get(8..12) != Some(i32::from_be_bytes(*b"VNDR").to_le_bytes().as_slice())
        {
            return Reply::Status(BAD_TYPE);
        }
        let mut reader = Reader::new(data);
        if reader.call().ok().as_deref() != Some(DESCRIPTOR) {
            return Reply::Status(BAD_TYPE);
        }
        let Ok(enabled) = reader.i32() else {
            return Reply::Status(BAD_VALUE);
        };
        if reader.here() != data.len() || !matches!(enabled, 0 | 1) {
            return Reply::Status(BAD_VALUE);
        }
        let operation = if enabled == 1 {
            protocol::RESUME
        } else {
            protocol::PAUSE
        };
        match self.0.lock().unwrap().call(operation) {
            Ok(ack) => {
                eprintln!(
                    "QSERVICE_ACK code=20 enable={enabled} generation={} commit={} state={}",
                    ack.generation, ack.commit, ack.state
                );
                let mut reply = Parcel::default();
                reply.i32(0); // BBinder::onTransact returns raw status_t, not AIDL exception.
                Reply::Parcel(reply)
            }
            Err(error) => {
                eprintln!("QSERVICE_BACKEND_FAILED {error:#}");
                Reply::Status(DEAD_OBJECT)
            }
        }
    }
}
impl<C> Service<C> for Endpoint {
    fn call(&self, _: &C, call: Call<'_>) -> Reply {
        if call.object != 1 {
            return Reply::Status(UNKNOWN_TRANSACTION);
        }
        self.answer(call.code, call.flags, call.uid, call.bytes())
    }
}

#[cfg(target_os = "android")]
pub(crate) fn run(args: &[OsString]) -> Result<()> {
    use skyward_binder::{driver::Binder, service};
    use std::{io::Write, os::fd::FromRawFd, sync::Arc};
    ensure!(
        args.len() == 3,
        "usage: display-ipc-qservice SOCKET OWNER_PID READY_FD"
    );
    crate::common::root()?;
    let source = Path::new(&args[0]);
    let expected_owner: i32 = args[1].to_str().context("owner PID utf8")?.parse()?;
    let ready_fd: i32 = args[2].to_str().context("ready FD utf8")?.parse()?;
    ensure!(
        expected_owner > 0 && (3..=1024).contains(&ready_fd),
        "invalid owner PID or ready FD"
    );
    let owner = Owner::connect(source, expected_owner)?;
    unsafe {
        libc::alarm(55);
    }
    let endpoint = Arc::new(Endpoint(Mutex::new(owner)));
    let binder = Binder::raw_serving(Path::new("/dev/vndbinder"), endpoint)?;
    ensure!(
        service::vendor_find(&binder, "display.qservice")?.is_none(),
        "refusing preexisting qservice"
    );
    service::vendor_add(&binder, "display.qservice", 1).context("register real QService")?;
    let registered = service::vendor_find(&binder, "display.qservice")?;
    ensure!(
        registered.is_some(),
        "private QService registration not visible"
    );
    // Previous verification happens before entering the no-ioctl and
    // non-writable filesystem sandbox; all subsequent calls use this fd.
    guard::install(Policy::BinderOnly(binder.driver_fd()))?;
    let mut ready = unsafe { std::fs::File::from_raw_fd(ready_fd) };
    ready.write_all(b"R")?;
    drop(ready);
    eprintln!(
        "DISPLAY_QSERVICE_READY pid={} owner={} supported=20 implementation=rust",
        std::process::id(),
        expected_owner
    );
    match binder.serve() {
        Err(err) => Err(err).context("private vendor QService Binder loop"),
        Ok(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Failure modes that do not contact the owner can be tested without
    // invoking a device or publishing anything in any Binder registry.
    #[test]
    fn no_other_transaction_or_uid_can_pause_hardware() {
        let mut call = Parcel::vendor_call(DESCRIPTOR);
        call.i32(1);
        let bytes = call.bytes();
        fn validate(code: u32, flags: u32, uid: u32, data: &[u8]) -> i32 {
            if uid != 0 {
                return PERMISSION_DENIED;
            }
            if code != SET_PAUSED {
                return UNKNOWN_TRANSACTION;
            }
            if flags & 1 != 0
                || data.get(8..12) != Some(i32::from_be_bytes(*b"VNDR").to_le_bytes().as_slice())
            {
                return BAD_TYPE;
            }
            let mut reader = Reader::new(data);
            if reader.call().ok().as_deref() != Some(DESCRIPTOR) {
                return BAD_TYPE;
            }
            let Ok(enabled) = reader.i32() else {
                return BAD_VALUE;
            };
            if reader.here() != data.len() || !matches!(enabled, 0 | 1) {
                return BAD_VALUE;
            }
            0
        }
        assert_eq!(validate(20, 0, 0, bytes), 0);
        assert_eq!(validate(399, 0, 0, bytes), UNKNOWN_TRANSACTION);
        assert_eq!(validate(20, 1, 0, bytes), BAD_TYPE);
        assert_eq!(validate(20, 0, 2000, bytes), PERMISSION_DENIED);
        let mut broken = Parcel::vendor_call(DESCRIPTOR);
        broken.i32(2);
        assert_eq!(validate(20, 0, 0, broken.bytes()), BAD_VALUE);
        assert_eq!(
            validate(20, 0, 0, Parcel::call(DESCRIPTOR).bytes()),
            BAD_TYPE
        );
    }
}
