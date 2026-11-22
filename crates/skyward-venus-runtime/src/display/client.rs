//! Private vendor Binder STC client, entirely Rust outside pinned OEM ELFs.
use super::{
    guard::{self, Policy},
    platform_binder, stc,
};
use anyhow::{Context, Result, ensure};
use skyward_binder::{driver::Binder, parcel::Parcel, service};
use std::{ffi::OsString, fs::File, io::Write, os::fd::FromRawFd, path::Path};
const QSERVICE: &str = "android.display.IQService";
fn exercise(binder: &Binder) -> Result<()> {
    let service = service::vendor_find(binder, "display.qservice")?
        .context("private display.qservice absent")?;
    for enabled in [0, 1] {
        let mut request = Parcel::vendor_call(QSERVICE);
        request.i32(enabled);
        let answer = service
            .transact(20, &request)
            .with_context(|| format!("real QService enable={enabled}"))?;
        let mut read = answer.read();
        ensure!(
            read.i32()? == 0 && read.here() == answer.bytes().len(),
            "bad QService confirmation"
        );
    }
    let unsupported = Parcel::vendor_call(QSERVICE);
    let e = service
        .transact(399, &unsupported)
        .err()
        .context("unknown transaction accepted")?;
    ensure!(
        format!("{e:#}").contains("-74"),
        "unknown transaction did not report UNKNOWN_TRANSACTION"
    );
    let mut invalid = Parcel::vendor_call(QSERVICE);
    invalid.i32(2);
    let e = service
        .transact(20, &invalid)
        .err()
        .context("invalid QService toggle accepted")?;
    ensure!(
        format!("{e:#}").contains("-22"),
        "invalid QService toggle did not report BAD_VALUE"
    );
    eprintln!("DISPLAY_QSERVICE_EXERCISE_PASS pause/resume=real-ACK invalid/unknown=rejected");
    Ok(())
}

pub(crate) fn run(args: &[OsString]) -> Result<()> {
    ensure!(
        args.len() == 1 && args[0] == "--list",
        "usage: display-ipc-client --list"
    );
    unsafe {
        libc::alarm(30);
    }
    stc::verify_vendor()?;
    let fd = unsafe { libc::dup(libc::STDOUT_FILENO) };
    ensure!(fd >= 0, "duplicate display output");
    let mut output = unsafe { File::from_raw_fd(fd) };
    ensure!(
        unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } >= 0,
        "redirect vendor stdout"
    );
    // Platform libbinder ProcessState must be configured before vendor plugins
    // create MI Binder objects. Raw Rust binder uses a separate fd to verify
    // private service-manager identity and exercise the actual QService.
    let platform = platform_binder::ProcessState::prepare()?;
    let binder = Binder::vendor_probe(Path::new("/dev/vndbinder"))?;
    ensure!(
        service::vendor_find(&binder, "display.qservice")?.is_some(),
        "original private QService absent"
    );
    ensure!(
        service::vendor_find(&binder, "display.mistcservice")?.is_none(),
        "preexisting MiStc service"
    );
    let platform_fd = platform_binder::bound_fd(Some(binder.driver_fd()))?;
    guard::install(Policy::BinderPair(binder.driver_fd(), platform_fd))?;
    platform.start_threads()?;
    eprintln!(
        "DISPLAY_IPC_GUARD_ACTIVE binder_rust_fd={} binder_platform_fd={} all_other_ioctl=EPERM writable_open=EPERM",
        binder.driver_fd(),
        platform_fd
    );
    exercise(&binder)?;
    let manager = stc::Library::load("/vendor/lib64/libsnapdragoncolor-manager.so")?;
    let mut api = stc::Interface::create(&manager)?;
    let init = api.initialize();
    eprintln!(
        "STC_INIT_RESULT status={}",
        if init.is_ok() { 0 } else { -1 }
    );
    init?;
    ensure!(
        service::vendor_find(&binder, "display.mistcservice")?.is_some(),
        "MI did not publish actual service"
    );
    let (_owned, modes) = api.modes()?;
    let bytes = stc::json_modes(&modes)?;
    output.write_all(&bytes)?;
    output.flush()?;
    eprintln!(
        "DISPLAY_IPC_LIST_DONE modes={} color_applied=false calibration_equivalence=unproven",
        modes.len()
    );
    // C++ plugin's Binder callback objects are owned by its service registry.
    // Do not unload plugin code, destroy the interface or detach its thread
    // pool before the process dies; _exit atomically tears down its lifetime.
    std::mem::forget(api);
    std::mem::forget(manager);
    unsafe { libc::_exit(0) }
}
