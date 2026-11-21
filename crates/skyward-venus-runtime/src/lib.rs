//! Venus recovery helpers. All mount and device operations are explicit Rust
//! implementations; Android/vendor executables retain their original ABI.
mod binder;
mod cache;
mod common;
mod crypto;
#[cfg(any(target_os = "android", test))]
mod display;
mod kmsg;
mod launcher;
mod modem;
mod modem_prepare;
#[cfg(any(target_os = "android", test))]
mod netd;
mod netmgr;
mod rpmb;
mod runtime;
#[cfg(target_os = "android")]
mod service_lock;
mod startup;
#[cfg(any(target_os = "android", test))]
mod suspend;
#[cfg(target_os = "android")]
mod suspend_probe;
use anyhow::{Context, Result, bail};
use std::{ffi::OsString, path::Path};

/// Process entrypoint, called before any worker threads are created.
pub fn run(mut args: Vec<OsString>) -> Result<()> {
    // Private receipts and logs inherit the same mask as the former runtime.
    unsafe {
        libc::umask(0o077);
    }

    let program = args.first().context("missing argv[0]")?;
    let name = Path::new(program)
        .file_name()
        .context("invalid argv[0]")?
        .to_string_lossy()
        .into_owned();
    args.remove(0);
    let role = if name == "venus-runtime" {
        if args.is_empty() {
            bail!("a helper name is required");
        }
        args.remove(0)
            .into_string()
            .map_err(|_| anyhow::anyhow!("non-UTF8 helper name"))?
    } else {
        name
    };
    match role.as_str() {
        #[cfg(target_os = "android")]
        "display-probe-launch" => display::root::probe_launch(&args),
        #[cfg(target_os = "android")]
        "display-ipc-launch" => display::ipc_launch(&args),
        #[cfg(target_os = "android")]
        "display-ipc-qservice" => display::qservice_run(&args),
        #[cfg(target_os = "android")]
        "display-probe" => display::probe_run(&args),
        #[cfg(target_os = "android")]
        "display-ipc-client" => display::client_run(&args),
        #[cfg(target_os = "android")]
        "network-netd-probe" => netd::probe(&args),
        #[cfg(target_os = "android")]
        "network-suspend-probe" => suspend_probe::run(&args),
        #[cfg(target_os = "android")]
        "network-netd" => {
            common::no_args(&args)?;
            netd::run()
        }
        #[cfg(target_os = "android")]
        "network-suspend" => {
            common::no_args(&args)?;
            suspend::run()
        }
        "modem-prepare" => {
            common::no_args(&args)?;
            modem_prepare::prepare()
        }
        "probe" => {
            common::no_args(&args)?;
            startup::probe()
        }
        "probe-logs" => {
            common::no_args(&args)?;
            startup::logs()
        }
        "ui-start" => {
            common::no_args(&args)?;
            startup::ui()
        }
        "security-start" => {
            common::no_args(&args)?;
            startup::security()
        }
        "binderfs-setup" => {
            common::no_args(&args)?;
            binder::setup()
        }
        "crypto-launch" => crypto::launch(&args),
        "km41-launch" => launcher::launch(&args),
        "km41-kmsg" => {
            common::no_args(&args)?;
            kmsg::capture()
        }
        "km41-rpmb" => rpmb::run(&args),
        "modem-hold" => {
            common::no_args(&args)?;
            modem::hold()
        }
        "netmgr-ready" => {
            common::no_args(&args)?;
            netmgr::ready()
        }
        "km41-runtime" => runtime::keymaster(&args),
        "ce-runtime" => runtime::gatekeeper(&args),
        "boot-runtime" => runtime::boot(&args),
        "crypto-start" => runtime::crypto(&args),
        "rw-runtime" => {
            common::no_args(&args)?;
            runtime::writable()
        }
        "cache-runtime" => {
            common::no_args(&args)?;
            cache::prepare()
        }
        _ => bail!("unsupported Venus helper: {role}"),
    }
}
