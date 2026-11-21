use crate::{binder, common::*};
use anyhow::{Result, bail, ensure};
use std::{ffi::OsString, fs, path::Path, process::Command};
pub fn launch(args: &[OsString]) -> Result<()> {
    root()?;
    let Some(role) = args.first().and_then(|s| s.to_str()) else {
        bail!("crypto role required")
    };
    if role == "setup-binder" {
        ensure!(args.len() == 1, "unexpected arguments");
        let base = Path::new("/dev/skyward-crypto-ipc");
        binder::mount_binder(base)?;
        return binder::node(base, "binder", 0o600);
    }
    let runtime = Path::new("/skyward/crypto-runtime");
    let mut command;
    if matches!(role, "manager" | "keystore2") {
        ensure!(
            args.len() == 1 || (args.len() == 2 && args[1] == "--list"),
            "unsupported arguments"
        );
        let base = Path::new(if role == "manager" {
            "/skyward/run/crypto/manager-root"
        } else {
            "/skyward/run/crypto/keystore-root"
        });
        directory(base, 0o755)?;
        private_mounts()?;
        for (source, dest, recursive, ro) in [
            (runtime.join("system"), "system", false, true),
            ("/dev".into(), "dev", true, false),
            ("/proc".into(), "proc", false, false),
            ("/sys".into(), "sys", true, false),
            ("/skyward/run/crypto/tmp".into(), "tmp", false, false),
        ] {
            directory(base.join(dest), 0o755)?;
            bind(&source, &base.join(dest), recursive, ro)?;
        }
        let manifest = Path::new("/skyward/runtime/framework-manifest.xml");
        let target = base.join("system/etc/vintf/manifest.xml");
        ensure!(
            fs::symlink_metadata(manifest)?.is_file() && fs::symlink_metadata(&target)?.is_file(),
            "crypto manifest file"
        );
        bind(manifest, &target, false, true)?;
        bind(
            Path::new("/dev/skyward-crypto-ipc/binder"),
            &base.join("dev/binderfs/binder"),
            false,
            false,
        )?;
        directory(base.join("vendor"), 0o755)?;
        directory(base.join("vendor/etc"), 0o755)?;
        bind(
            Path::new("/skyward/run/hal/cache/vendor/etc"),
            &base.join("vendor/etc"),
            false,
            true,
        )?;
        detach_private_sources()?;
        chroot(base)?;
        command = Command::new("/system/bin/linker64");
        command
            .env("ANDROID_ROOT", "/system")
            .env("ANDROID_DATA", "/data")
            .env("TMPDIR", "/tmp")
            .env("LD_LIBRARY_PATH", "/system/lib64");
        if args.len() == 2 {
            command.arg("--list");
        }
        command.arg(if role == "manager" {
            "/system/bin/servicemanager"
        } else {
            "/system/bin/keystore2"
        });
        if role == "keystore2" && args.len() == 1 {
            command.arg("/tmp/misc/keystore");
        }
    } else {
        ensure!(args.len() <= 19, "too many arguments");
        let program = match role {
            "prepare-subdirs" => "vold_prepare_subdirs",
            "restorecon" => "toybox",
            "fsck.f2fs" => "fsck.f2fs",
            "e2fsck" => "e2fsck",
            _ => bail!("unsupported crypto role"),
        };
        command = Command::new(runtime.join("system/bin/linker64"));
        command
            .env("LD_LIBRARY_PATH", runtime.join("system/lib64"))
            .arg(runtime.join("system/bin").join(program));
        if role == "restorecon" {
            command.arg("restorecon");
        }
        command.args(&args[1..]);
    }
    command
        .env_remove("LD_PRELOAD")
        .env_remove("LD_CONFIG_FILE");
    exec(&mut command)
}
