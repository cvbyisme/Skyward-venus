//! Standalone, one-shot Rust STC color probe, no KMS or framebuffer access.
use super::{
    guard::{self, Policy},
    platform_binder, stc,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::OsString,
    fs::File,
    io::{Write, stdout},
    os::fd::FromRawFd,
};

pub(crate) fn run(args: &[OsString]) -> Result<()> {
    let (render, index, id) = match args {
        [mode] if mode == "--list" => (false, 0, 0),
        [mode, index, id] if mode == "--render" => {
            let index: u32 = index.to_str().context("index UTF8")?.parse()?;
            let id: u32 = id.to_str().context("QDCM mode ID UTF8")?.parse()?;
            ensure!(
                index <= i32::MAX as u32 && id <= i32::MAX as u32,
                "render index or ID exceeds int32"
            );
            (true, index as usize, id)
        }
        _ => bail!("usage: display-probe --list | --render LIST_INDEX QDCM_MODE_ID"),
    };
    unsafe {
        libc::alarm(30);
    }
    stc::verify_vendor()?;
    let fd = unsafe { libc::dup(libc::STDOUT_FILENO) };
    ensure!(
        fd >= 0,
        "duplicate display output: {}",
        std::io::Error::last_os_error()
    );
    let mut output = unsafe { File::from_raw_fd(fd) };
    ensure!(
        unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } >= 0,
        "redirect vendor stdout"
    );
    // Platform ProcessState (not a stand-in Binder driver) is required by
    // vendor constructors. The first filter allows only driver VERSION and
    // SET_MAX_THREADS; the second denies *all* ioctls prior to dlopen.
    guard::install(Policy::InitializeBinder)?;
    eprintln!("BINDER_INIT_BEGIN driver=/dev/vndbinder allowed_ioctl=VERSION,SET_MAX_THREADS");
    let _platform = platform_binder::ProcessState::prepare()?;
    eprintln!("BINDER_INIT_DONE driver=/dev/vndbinder thread_pool=not_started");
    guard::install(Policy::NoIoctl)?;
    eprintln!("DISPLAY_IO_GUARD_ACTIVE all_ioctl=EPERM writable_open=EPERM");
    let manager = stc::Library::load("/vendor/lib64/libsnapdragoncolor-manager.so")?;
    let mut api = stc::Interface::create(&manager)?;
    eprintln!("STC_PANEL normalized=xiaomi_38_08_0a_cmd_mode_dsc_dsi_panel");
    api.initialize()?;
    let (_owned_list, modes) = api.modes()?;
    let data = if render {
        ensure!(
            index < modes.len(),
            "mode index outside library-reported list"
        );
        let records = api.render(&modes[index], index, id)?;
        stc::frame(&records)?
    } else {
        stc::json_modes(&modes)?
    };
    output.write_all(&data)?;
    output.flush()?;
    api.deinitialize()?;
    let _ = stdout().flush();
    Ok(())
}
