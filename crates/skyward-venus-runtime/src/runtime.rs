//! Recovery service lifecycle and readiness. No shell interpreter is used.
use crate::common::*;
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};
pub const HAL: &str = "/skyward/run/hal";
pub const ASSETS: &str = "/skyward/runtime";
const STATE: &str = "/skyward/run/state/recovery";
pub const SKYWARD: &str = "/system/bin/skyward";
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.env("PATH", "/system/bin:/system/xbin");
    for key in ["TMPDIR", "TMP", "TEMP"] {
        cmd.env(key, "/skyward/run/tmp");
    }
    cmd
}
pub fn checked(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Result<()> {
    let program = program.as_ref();
    let status = command(program)
        .args(args)
        .status()
        .with_context(|| format!("start {}", program.to_string_lossy()))?;
    ensure!(
        status.success(),
        "{} failed: {status}",
        program.to_string_lossy()
    );
    Ok(())
}
pub fn output(program: &str, args: &[&str]) -> Result<String> {
    let out = command(program).args(args).output()?;
    ensure!(out.status.success(), "{program} failed: {}", out.status);
    Ok(String::from_utf8(out.stdout)?
        .trim_end_matches(['\n', '\r'])
        .into())
}
pub fn prop(key: &str) -> Result<String> {
    output("/system/bin/getprop", &[key])
}
fn set_prop(key: &str, value: &str) -> Result<()> {
    checked("/system/bin/setprop", &[key, value])
}
pub fn log(path: impl AsRef<Path>, append: bool) -> Result<File> {
    Ok(OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?)
}
pub fn redirect(path: impl AsRef<Path>, append: bool) -> Result<()> {
    io::stdout().flush()?;
    io::stderr().flush()?;
    let f = log(path, append)?;
    cv(unsafe { libc::dup2(f.as_raw_fd(), 1) })?;
    cv(unsafe { libc::dup2(f.as_raw_fd(), 2) })?;
    Ok(())
}
pub fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        digest.update(&bytes[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub fn verify(path: &Path, expected: &str) -> Result<()> {
    ensure!(
        hash(path)? == expected,
        "SHA256 mismatch: {}",
        path.display()
    );
    Ok(())
}
pub fn verify_manifest(base: &Path, name: &str) -> Result<()> {
    let manifest = fs::read_to_string(base.join(name))?;
    for line in manifest.lines() {
        let (digest, path) = line.split_once("  ").context("malformed SHA256 manifest")?;
        ensure!(
            digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
            "bad SHA256 digest"
        );
        let relative = Path::new(path);
        ensure!(
            !relative.is_absolute()
                && relative
                    .components()
                    .all(|c| !matches!(c, std::path::Component::ParentDir)),
            "unsafe manifest path"
        );
        verify(&base.join(relative), digest)?;
    }
    ensure!(!manifest.is_empty(), "empty SHA256 manifest");
    Ok(())
}
pub fn mounted(path: &str) -> Result<bool> {
    Ok(fs::read_to_string("/proc/mounts")?
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some(path)))
}
pub fn integrated() -> Result<()> {
    root()?;
    ensure!(
        fs::read_to_string("/skyward/venus-recovery.txt")?
            .lines()
            .next()
            == Some("venus-recovery-rw-v1"),
        "requires integrated Recovery"
    );
    Ok(())
}
fn no_data() -> Result<()> {
    ensure!(
        !mounted("/data")?,
        "refusing to recreate runtime with live Data"
    );
    Ok(())
}
fn permissive() -> Result<()> {
    ensure!(
        output("/system/bin/getenforce", &[])? == "Permissive",
        "diagnostic HAL setup is not validated in enforcing mode"
    );
    Ok(())
}
pub struct SetupLock(PathBuf);
impl SetupLock {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let p = path.into();
        fs::create_dir(&p).context("setup already active or interrupted")?;
        Ok(Self(p))
    }
}
impl Drop for SetupLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}
pub fn pid(base: &Path, role: &str) -> Result<libc::pid_t> {
    let s = fs::read_to_string(base.join(format!("{role}.pid")))?;
    let value: i32 = s.trim().parse()?;
    ensure!(
        value > 1 && s.trim().bytes().all(|b| b.is_ascii_digit()),
        "invalid service PID"
    );
    Ok(value)
}
pub fn running(base: &Path, role: &str) -> bool {
    pid(base, role).is_ok_and(|p| unsafe { libc::kill(p, 0) } == 0)
}
pub fn alive(base: &Path, role: &str, root: &Path) -> bool {
    pid(base, role).is_ok_and(|p| unsafe { libc::kill(p, 0) } == 0 && fs::read_link(format!("/proc/{p}/root")).is_ok_and(|p| p == root))
}
fn hal_alive(role: &str) -> bool {
    alive(
        Path::new(HAL),
        role,
        &Path::new(HAL).join(format!("root-{role}")),
    )
}
fn no_foreign(name: &str) -> Result<()> {
    let status = command("/system/bin/pidof")
        .arg(name)
        .stdout(Stdio::null())
        .status()?;
    ensure!(
        status.code() == Some(1),
        "existing or unqueryable unowned service: {name}"
    );
    Ok(())
}
fn start_role(base: &Path, launcher: &Path, role: &str) -> Result<()> {
    let file = log(base.join(format!("{role}.log")), false)?;
    let mut cmd = command(launcher);
    cmd.arg(role)
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file);
    unsafe {
        cmd.pre_exec(|| {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    fs::write(
        base.join(format!("{role}.pid")),
        format!("{}\n", child.id()),
    )?;
    // The init-owned supervisor intentionally retains the process group;
    // the launching helper may return without stopping these long-lived HALs.
    Ok(())
}
pub fn query(program: &Path, args: &[&str], file: &Path, seconds: u64) -> Result<bool> {
    let out = log(file, false)?;
    let mut child = command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .spawn()?;
    let until = Instant::now() + Duration::from_secs(seconds);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if Instant::now() >= until {
            child.kill()?;
            child.wait()?;
            return Ok(false);
        }
        sleep(Duration::from_millis(20));
    }
}
pub fn mark(phase: &str) {
    let uptime = fs::read_to_string("/proc/uptime").unwrap_or_default();
    println!(
        "VENUS_RUNTIME_PHASE {phase} boot_seconds={}",
        uptime.split_whitespace().next().unwrap_or("unknown")
    );
}
fn wait_property(role: &str, key: &str) -> Result<()> {
    for _ in 0..200 {
        // A fresh launcher needs time to complete its private chroot, but
        // the kernel must still report the owned process alive throughout.
        ensure!(
            running(Path::new(HAL), role),
            "HAL exited before ready: {role}"
        );
        if hal_alive(role) && prop(key)? == "true" {
            println!("KEYMASTER_DEPENDENCY_READY role={role} property={key}");
            return Ok(());
        }
        sleep(Duration::from_millis(100));
    }
    bail!("timed out waiting for HAL readiness: {role}/{key}")
}
pub fn property_from(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name == key).then(|| value.to_owned())
    })
}
fn reset(client: &Path, key: &str, value: &str) -> Result<()> {
    let status = command(client)
        .arg0("resetprop")
        .args(["-n", key, value])
        .status()?;
    ensure!(status.success(), "reset property {key}");
    Ok(())
}
pub fn keymaster(args: &[OsString]) -> Result<()> {
    recovery()?;
    permissive()?;
    no_data()?;
    ensure!(args.len() <= 1, "use start or stop");
    let action = args.first().and_then(|s| s.to_str()).unwrap_or("start");
    fs::create_dir_all(HAL)?;
    directory(HAL, 0o700)?;
    if action == "stop" {
        for role in ["gatekeeper", "keymaster", "hwmanager", "qseecomd"] {
            if hal_alive(role) {
                cv(unsafe { libc::kill(pid(Path::new(HAL), role)?, libc::SIGTERM) })?;
            }
        }
        println!(
            "Owned HAL processes were asked to stop. Binder endpoints and read-only mounts remain for inspection."
        );
        return Ok(());
    }
    ensure!(action == "start", "use start or stop");
    let _lock = SetupLock::new(Path::new(HAL).join("setup.lock"))?;
    let stage = Path::new("/skyward/run/hal/stage-km41");
    let client = stage.join("skyward");
    if hal_alive("keymaster") {
        ensure!(
            hal_alive("hwmanager")
                && hal_alive("qseecomd")
                && prop("hwservicemanager.ready")? == "true"
                && prop("vendor.sys.listeners.registered")? == "true",
            "owned Keymaster lost dependency"
        );
        crate::rpmb::run(&["--apply".into()])?;
        ensure!(
            query(
                &client,
                &["keymaster41"],
                &Path::new(HAL).join("hardware.txt"),
                10
            )?,
            "Keymaster query failed"
        );
        return Ok(());
    }
    for role in ["keymaster", "hwmanager", "qseecomd"] {
        ensure!(!hal_alive(role), "partial setup remains: {role}");
    }
    for name in [
        "hwservicemanager",
        "android.hardware.keymaster@4.1-service-qti",
        "qseecomd",
    ] {
        no_foreign(name)?;
    }
    mark("SOURCE_MOUNTS_BEGIN");
    ensure!(
        query(
            &client,
            &["venus-basic", "status"],
            &Path::new(HAL).join("partition-status.txt"),
            20
        )?,
        "partition status failed"
    );
    for part in ["system", "system_ext", "vendor", "odm", "product"] {
        checked(&client, &["venus-basic", "mount", "--name", part])?;
    }
    mark("SOURCE_MOUNTS_READY");
    crate::rpmb::run(&["--apply".into()])?;
    for (path, digest) in [
        (
            PathBuf::from("/mnt/skyward-ro/system/system/apex/com.android.runtime.apex"),
            "021646695ba127260ea9cc96df6c924edfeb86ba783781a9cc276db232443fc4",
        ),
        (
            PathBuf::from("/mnt/skyward-ro/system_ext/apex/com.android.vndk.v30.apex"),
            "eb7dd17338eae20c8ea897f878a74ff519f24481eeacf13be11b895eb8a9cc38",
        ),
        (
            stage.join("runtime.img"),
            "9776b642132872ce4931218694c345231ecaac4490ead5d7c2f7c24e17481c17",
        ),
        (
            stage.join("vndk30.img"),
            "8948745de8d27cae3fbdf23f6185ae4b6aec680b6a6febf2a2f0eaba1ab59b9e",
        ),
    ] {
        verify(&path, digest)?;
    }
    mark("IMAGES_VERIFY_READY");
    for (name, image, kind, options) in [
        (
            "apex-runtime",
            "runtime.img",
            "erofs",
            "ro,loop,nosuid,nodev",
        ),
        (
            "apex-vndk30",
            "vndk30.img",
            "ext4",
            "ro,loop,noload,nosuid,nodev",
        ),
    ] {
        let target = Path::new(HAL).join(name);
        fs::create_dir_all(&target)?;
        if !mounted(target.to_str().unwrap())? {
            checked(
                "/system/bin/mount",
                &[
                    "-t",
                    kind,
                    "-o",
                    options,
                    stage.join(image).to_str().unwrap(),
                    target.to_str().unwrap(),
                ],
            )?;
        }
    }
    if Path::new("/skyward/venus-recovery.txt").exists() {
        crate::cache::prepare()?;
        checked(&client, &["venus-firmware"])?;
    }
    mark("FIRMWARE_READY");
    crate::binder::setup()?;
    fs::copy(
        stage.join("framework-manifest.xml"),
        Path::new(HAL).join("framework-manifest.xml"),
    )?;
    fs::set_permissions(
        Path::new(HAL).join("framework-manifest.xml"),
        fs::Permissions::from_mode(0o644),
    )?;
    for node in ["/dev/ion", "/dev/qseecom", "/dev/smcinvoke"] {
        ensure!(
            fs::metadata(node)?.file_type().is_char_device(),
            "missing device {node}"
        );
    }
    own(Path::new("/dev/ion"), 1000, 1000, 0o664)?;
    for p in ["/dev/qseecom", "/dev/smcinvoke"] {
        own(Path::new(p), 1000, 1026, 0o660)?;
    }
    let system = fs::read_to_string("/mnt/skyward-ro/system/system/build.prop")?;
    let vendor = fs::read_to_string("/mnt/skyward-ro/vendor/build.prop")?;
    for (key, text) in [
        ("ro.build.version.release", &system),
        ("ro.build.version.security_patch", &system),
        ("ro.vendor.build.security_patch", &vendor),
    ] {
        let actual = property_from(text, key)
            .filter(|v| !v.is_empty())
            .with_context(|| format!("missing actual {key}"))?;
        reset(&client, key, &actual)?;
    }
    let odm = fs::read_to_string("/mnt/skyward-ro/odm/etc/build.prop").unwrap_or_default();
    for key in [
        "ro.crypto.dm_default_key.options_format.version",
        "ro.crypto.volume.metadata.method",
        "ro.crypto.volume.options",
        "external_storage.casefold.enabled",
        "external_storage.sdcardfs.enabled",
    ] {
        let actual = property_from(&vendor, key)
            .filter(|v| !v.is_empty())
            .or_else(|| property_from(&odm, key).filter(|v| !v.is_empty()))
            .unwrap_or_default();
        reset(&client, key, &actual)?;
    }
    for (k, v) in [
        ("hwservicemanager.disabled", "false"),
        ("vendor.gatekeeper.disable_spu", "true"),
        ("hwservicemanager.ready", "false"),
        ("vendor.sys.listeners.registered", "false"),
    ] {
        set_prop(k, v)?;
    }
    mark("SECURITY_HAL_START_BEGIN");
    let launch = stage.join("km41-launch");
    start_role(Path::new(HAL), &launch, "hwmanager")?;
    wait_property("hwmanager", "hwservicemanager.ready")?;
    start_role(Path::new(HAL), &launch, "qseecomd")?;
    wait_property("qseecomd", "vendor.sys.listeners.registered")?;
    start_role(Path::new(HAL), &launch, "keymaster")?;
    let until = Instant::now() + Duration::from_secs(60);
    while Instant::now() < until {
        if query(
            &client,
            &["keymaster41"],
            &Path::new(HAL).join("hardware.txt"),
            5,
        )? {
            println!("KEYMASTER41_RUNTIME_READY; metadata/DE tests require explicit commands");
            return Ok(());
        }
        sleep(Duration::from_millis(100));
    }
    bail!("HAL setup did not register successfully; data has not been opened")
}
pub fn gatekeeper(args: &[OsString]) -> Result<()> {
    recovery()?;
    no_data()?;
    permissive()?;
    ensure!(args.len() <= 1, "unknown CE runtime mode");
    match args
        .first()
        .and_then(|s| s.to_str())
        .unwrap_or("diagnostic")
    {
        "diagnostic" => ensure!(
            Path::new("/skyward/run/state/data-ro.txt").is_file(),
            "retained Data session required"
        ),
        "full-recovery" => {
            integrated()?;
            ensure!(
                !Path::new("/skyward/run/state/data-ro.txt").exists(),
                "old RO session exists"
            );
        }
        _ => bail!("unknown CE runtime mode"),
    }
    for role in ["hwmanager", "qseecomd", "keymaster"] {
        ensure!(
            hal_alive(role),
            "required owned service unavailable: {role}"
        );
    }
    ensure!(
        prop("hwservicemanager.ready")? == "true"
            && prop("vendor.sys.listeners.registered")? == "true",
        "HAL dependencies not ready"
    );
    crate::rpmb::run(&["--apply".into()])?;
    let _lock = SetupLock::new(Path::new(HAL).join("ce-setup.lock"))?;
    if !hal_alive("gatekeeper") {
        no_foreign("android.hardware.gatekeeper@1.0-service-qti")?;
        start_role(
            Path::new(HAL),
            Path::new("/skyward/run/hal/stage-km41/km41-launch"),
            "gatekeeper",
        )?;
    }
    for _ in 0..10 {
        if query(
            Path::new("/skyward/run/hal/stage-ce/skyward"),
            &["gatekeeper", "--hidl"],
            &Path::new(HAL).join("gatekeeper-query.txt"),
            3,
        )? {
            ensure!(hal_alive("gatekeeper"), "Gatekeeper exited after query");
            println!("VENUS_CE_RUNTIME_READY; credential_submitted=false");
            return Ok(());
        }
        sleep(Duration::from_secs(1));
    }
    bail!("Gatekeeper readiness query failed; no credential submitted")
}
fn wait_service(name: &str, role: &str, registered: bool) -> Result<()> {
    let path = Path::new(HAL).join("control-readiness.txt");
    for _ in 0..20 {
        ensure!(
            running(Path::new(HAL), "servicemanager") && running(Path::new(HAL), role),
            "control runtime exited: {role}"
        );
        if hal_alive("servicemanager")
            && hal_alive(role)
            && query(Path::new(SKYWARD), &["service", name], &path, 2)?
        {
            let text = fs::read_to_string(&path)?;
            if text.lines().any(|s| {
                if registered {
                    s.ends_with("is described and registered")
                } else {
                    s.contains("is described and ")
                }
            }) {
                return Ok(());
            }
        }
        sleep(Duration::from_secs(1));
    }
    bail!("control runtime not ready: {role}")
}
pub fn boot(args: &[OsString]) -> Result<()> {
    integrated()?;
    ensure!(
        args.is_empty() || (args.len() == 1 && args[0] == "--binder-only"),
        "invalid boot runtime argument"
    );
    let binder_only = !args.is_empty();
    let base = Path::new(HAL);
    ensure!(
        !mounted("/data")? || base.join("servicemanager.pid").is_file(),
        "refusing new Binder manager with live Data"
    );
    ensure!(
        fs::read_to_string("/sys/devices/soc0/soc_id")?.trim() == "415",
        "unexpected SoC"
    );
    ensure!(
        fs::read_to_string("/mnt/skyward-ro/vendor/bin/init.qti.qcv.sh")?
            .contains("setprop ro.vendor.qti.soc_name lahaina")
            && Path::new("/mnt/skyward-ro/vendor/etc/vintf/manifest_lahaina.xml").is_file(),
        "installed vendor SKU not verified"
    );
    match prop("ro.boot.product.vendor.sku")?.as_str() {
        "" => set_prop("ro.boot.product.vendor.sku", "lahaina")?,
        "lahaina" => {}
        _ => bail!("unexpected installed vendor SKU"),
    }
    let text = fs::read_to_string("/mnt/skyward-ro/system/system/build.prop")?;
    let sdk = property_from(&text, "ro.build.version.sdk").context("missing SDK")?;
    ensure!(
        sdk.bytes().all(|b| b.is_ascii_digit()) && (30..=99).contains(&sdk.parse::<u32>()?),
        "invalid SDK"
    );
    set_prop("skyward.servicemanager.sdk", &sdk)?;
    set_prop("skyward.binder.domain", "system")?;
    let launcher = Path::new(ASSETS).join("km41-launch");
    if base.join("servicemanager.pid").exists() {
        ensure!(
            hal_alive("servicemanager"),
            "owned Binder manager exited; no automatic restart"
        );
    } else {
        no_foreign("servicemanager")?;
        start_role(base, &launcher, "servicemanager")?;
    }
    let name = if binder_only {
        "android.system.keystore2.IKeystoreService/default"
    } else {
        "android.hardware.boot.IBootControl/default"
    };
    wait_service(name, "servicemanager", false)?;
    if binder_only {
        return Ok(());
    }
    ensure!(
        !base.join("boot.pid").exists(),
        "BootControl already has an owner"
    );
    start_role(base, &launcher, "boot")?;
    wait_service(name, "boot", true)?;
    ensure!(
        query(
            Path::new(SKYWARD),
            &["slot"],
            &base.join("boot-slots.txt"),
            5
        )?,
        "BootControl slot query failed"
    );
    println!(
        "VENUS_BOOTCONTROL_READY sdk={sdk} vendor_sku=lahaina domain=system registration=verified slot_write=false"
    );
    Ok(())
}
pub fn crypto(args: &[OsString]) -> Result<()> {
    ensure!(
        args.is_empty() || (args.len() == 1 && args[0] == "--verify-assets"),
        "invalid crypto argument"
    );
    let base = Path::new("/skyward/run/crypto");
    for p in ["", "tmp", "tmp/misc", "tmp/misc/keystore"] {
        fs::create_dir_all(base.join(p))?;
        directory(base.join(p), 0o700)?;
    }
    let verified = verify_manifest(Path::new("/skyward/crypto-runtime"), "SOURCE.sha256");
    fs::write(
        base.join("verify.log"),
        match &verified {
            Ok(()) => "All reference assets verified\n".into(),
            Err(e) => format!("{e:#}\n"),
        },
    )?;
    verified?;
    if !args.is_empty() {
        return Ok(());
    }
    ensure!(
        !base.join("manager.pid").exists() && !base.join("keystore2.pid").exists(),
        "crypto service already has an owner; no automatic restart"
    );
    crate::crypto::launch(&["setup-binder".into()])?;
    let launcher = Path::new(ASSETS).join("crypto-launch");
    ensure!(
        query(
            &launcher,
            &["manager", "--list"],
            &base.join("manager-linker.log"),
            10
        )?,
        "manager linker check failed"
    );
    start_role(base, &launcher, "manager")?;
    let mut manager_ready = false;
    for _ in 0..20 {
        ensure!(running(base, "manager"), "crypto Binder manager exited");
        if query(
            Path::new(SKYWARD),
            &["service", "--crypto", "android.security.compat"],
            &base.join("manager-readiness.txt"),
            2,
        )? {
            manager_ready = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    ensure!(manager_ready, "crypto Binder manager readiness timeout");
    ensure!(
        query(
            &launcher,
            &["keystore2", "--list"],
            &base.join("linker.log"),
            10
        )?,
        "keystore2 linker check failed"
    );
    start_role(base, &launcher, "keystore2")?;
    for _ in 0..30 {
        ensure!(running(base, "keystore2"), "keystore2 exited");
        let mut ready = true;
        for (service, log) in [
            (
                "android.system.keystore2.IKeystoreService/default",
                "readiness.txt",
            ),
            ("android.security.authorization", "authorization.txt"),
        ] {
            let file = base.join(log);
            if !query(
                Path::new(SKYWARD),
                &["service", "--crypto", service],
                &file,
                2,
            )? || !fs::read_to_string(file)?
                .lines()
                .any(|s| s.ends_with("and registered"))
            {
                ready = false;
                break;
            }
        }
        if ready {
            println!("VENUS_KEYSTORE2_READY");
            return Ok(());
        }
        sleep(Duration::from_secs(1));
    }
    bail!("keystore2 registration timeout")
}
pub fn writable() -> Result<()> {
    integrated()?;
    fs::create_dir_all(STATE)?;
    directory(STATE, 0o700)?;
    let state = Path::new(STATE);
    if startup_preflight(
        state,
        Path::new("/skyward/run/state/data-ro.txt").exists(),
        mounted("/data")?,
    )? {
        for role in [
            "hwmanager",
            "qseecomd",
            "keymaster",
            "gatekeeper",
            "servicemanager",
        ] {
            ensure!(
                hal_alive(role),
                "HAL exited: {role}; no automatic restart on live Data"
            );
        }
        for (role, root) in [("keystore2", "keystore-root"), ("manager", "manager-root")] {
            ensure!(
                alive(
                    Path::new("/skyward/run/crypto"),
                    role,
                    &Path::new("/skyward/run/crypto").join(root)
                ),
                "crypto HAL exited: {role}; no automatic restart"
            );
        }
        ensure!(
            prop("hwservicemanager.ready")? == "true"
                && prop("vendor.sys.listeners.registered")? == "true"
                && prop("skyward.binder.domain")? == "system",
            "HAL readiness lost"
        );
        return crate::rpmb::run(&["--check".into()]);
    }
    redirect(state.join("runtime.log"), true)?;
    fresh_setup(state, |step| match step {
        Step::Assets => verify_manifest(Path::new(ASSETS), "SHA256SUMS"),
        Step::CryptoAssets => crypto(&["--verify-assets".into()]),
        Step::Stage => stage_assets(),
        Step::Keymaster => keymaster(&[]),
        Step::Gatekeeper => gatekeeper(&["full-recovery".into()]),
        Step::Binder => boot(&["--binder-only".into()]),
        Step::Keystore => crypto(&[]),
        Step::Boot => boot(&[]),
    })
}
fn stage_assets() -> Result<()> {
    for stage in ["stage-km41", "stage-ce"] {
        let stage = Path::new(HAL).join(stage);
        fs::create_dir_all(&stage)?;
        directory(&stage, 0o700)?;
        for name in [
            "binderfs-setup",
            "km41-launch",
            "km41-rpmb",
            "km41-kmsg",
            "runtime.img",
            "vndk30.img",
            "framework-manifest.xml",
            "km41-runtime",
            "ce-runtime",
            "skyward",
        ] {
            let src = if name == "skyward" {
                PathBuf::from(SKYWARD)
            } else {
                Path::new(ASSETS).join(name)
            };
            let dst = stage.join(name);
            match fs::symlink_metadata(&dst) {
                Ok(m) => ensure!(
                    m.is_symlink() && fs::read_link(&dst)? == src,
                    "foreign runtime asset: {name}"
                ),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    std::os::unix::fs::symlink(src, dst)?
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}
/// True means verify the existing owner; never restart it against live Data.
fn startup_preflight(state: &Path, old_ro: bool, data_mounted: bool) -> Result<bool> {
    if state.join("runtime-ready").exists() || state.join("crypto-ready").exists() {
        for (file, expected) in [
            ("crypto-ready", "venus-crypto-ready-v1"),
            ("runtime-ready", "venus-runtime-ready-v2"),
        ] {
            if state.join(file).exists() {
                ensure!(
                    fs::read_to_string(state.join(file))?.trim() == expected,
                    "older runtime remains; boot fresh image"
                );
            }
        }
        return Ok(true);
    }
    ensure!(
        !state.join("setup-failed").exists(),
        "previous security setup failed; boot fresh recovery"
    );
    ensure!(!old_ro, "existing RO diagnostic session");
    ensure!(!data_mounted, "refusing to recreate runtime with live Data");
    Ok(false)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Assets,
    CryptoAssets,
    Stage,
    Keymaster,
    Gatekeeper,
    Binder,
    Keystore,
    Boot,
}
impl Step {
    fn phase(self) -> &'static str {
        match self {
            Self::Assets => "ASSETS_VERIFY_BEGIN",
            Self::CryptoAssets => "CRYPTO_ASSETS_VERIFY_BEGIN",
            Self::Stage => "STAGE_BEGIN",
            Self::Keymaster => "KEYMASTER_START_BEGIN",
            Self::Gatekeeper => "GATEKEEPER_START_BEGIN",
            Self::Binder => "BINDER_START_BEGIN",
            Self::Keystore => "KEYSTORE2_START_BEGIN",
            Self::Boot => "BOOTCONTROL_START_BEGIN",
        }
    }
}
fn fresh_setup(state: &Path, mut execute: impl FnMut(Step) -> Result<()>) -> Result<()> {
    let _lock = SetupLock::new(state.join("setup.lock"))?;
    for step in [
        Step::Assets,
        Step::CryptoAssets,
        Step::Stage,
        Step::Keymaster,
        Step::Gatekeeper,
        Step::Binder,
        Step::Keystore,
    ] {
        mark(step.phase());
        if let Err(error) = execute(step) {
            fs::write(state.join("setup-failed"), format!("{}\n", step.phase()))?;
            return Err(error);
        }
    }
    fs::write(state.join("crypto-ready"), "venus-crypto-ready-v1\n")?;
    match execute(Step::Boot) {
        Ok(()) => {
            mark("BOOTCONTROL_READY");
            fs::write(state.join("runtime-ready"), "venus-runtime-ready-v2\n")?;
        }
        Err(e) => {
            eprintln!("BootControl unavailable; crypto runtime remains owned and ready: {e:#}")
        }
    }
    println!("VENUS_RECOVERY_RUNTIME_READY; Data opening is a separate worker step");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_order_and_boot_failure_do_not_discard_ready_crypto() {
        for fail_boot in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let state = tmp.path();
            let mut calls = Vec::new();
            assert!(!startup_preflight(state, false, false).unwrap());
            fresh_setup(state, |s| {
                calls.push(s);
                if fail_boot && s == Step::Boot {
                    bail!("boot query failed");
                }
                Ok(())
            })
            .unwrap();
            assert_eq!(
                calls,
                [
                    Step::Assets,
                    Step::CryptoAssets,
                    Step::Stage,
                    Step::Keymaster,
                    Step::Gatekeeper,
                    Step::Binder,
                    Step::Keystore,
                    Step::Boot
                ]
            );
            assert!(state.join("crypto-ready").is_file());
            assert_eq!(state.join("runtime-ready").exists(), !fail_boot);
            assert!(!state.join("setup.lock").exists());
            assert!(!state.join("setup-failed").exists());
            assert!(
                startup_preflight(state, false, true).unwrap(),
                "ready live Data must revalidate, never restart"
            );
        }
    }
    #[test]
    fn every_failed_stage_stops_following_services_and_refuses_a_retry() {
        for fail in [
            Step::Assets,
            Step::CryptoAssets,
            Step::Stage,
            Step::Keymaster,
            Step::Gatekeeper,
            Step::Binder,
            Step::Keystore,
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let state = tmp.path();
            let mut calls = Vec::new();
            assert!(
                fresh_setup(state, |s| {
                    calls.push(s);
                    if s == fail {
                        bail!("fixture failure");
                    }
                    Ok(())
                })
                .is_err()
            );
            assert_eq!(calls.last(), Some(&fail));
            assert!(!calls.contains(&Step::Boot));
            assert_eq!(
                fs::read_to_string(state.join("setup-failed")).unwrap(),
                format!("{}\n", fail.phase())
            );
            assert!(!state.join("crypto-ready").exists());
            assert!(!state.join("runtime-ready").exists());
            assert!(!state.join("setup.lock").exists());
            assert!(startup_preflight(state, false, false).is_err());
        }
    }
    #[test]
    fn foreign_live_data_and_old_runtime_cannot_be_adopted() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        assert!(startup_preflight(state, true, false).is_err());
        assert!(startup_preflight(state, false, true).is_err());
        fs::write(state.join("runtime-ready"), "venus-runtime-ready-v1\n").unwrap();
        assert!(startup_preflight(state, false, true).is_err());
        assert_eq!(
            fs::read_to_string(state.join("runtime-ready")).unwrap(),
            "venus-runtime-ready-v1\n"
        );
    }
    #[test]
    fn existing_setup_lock_prevents_all_steps() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("setup.lock")).unwrap();
        assert!(
            fresh_setup(tmp.path(), |_| panic!(
                "must not execute against another owner"
            ))
            .is_err()
        );
        assert!(tmp.path().join("setup.lock").is_dir());
    }
    #[test]
    fn properties_are_data_not_shell() {
        let text = "# key=wrong\nkey.other=wrong\nkey=$(touch /bad)=literal\r\nlast=tail";
        assert_eq!(
            property_from(text, "key").as_deref(),
            Some("$(touch /bad)=literal")
        );
        assert_eq!(property_from(text, "last").as_deref(), Some("tail"));
        assert!(property_from(text, "missing").is_none());
    }
    #[test]
    fn manifest_checks_content_and_refuses_parent_escape() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("payload"), b"abc").unwrap();
        fs::write(
            tmp.path().join("sums"),
            format!("{}  payload\n", hash(&tmp.path().join("payload")).unwrap()),
        )
        .unwrap();
        verify_manifest(tmp.path(), "sums").unwrap();
        fs::write(tmp.path().join("payload"), b"corrupt").unwrap();
        assert!(verify_manifest(tmp.path(), "sums").is_err());
        fs::write(
            tmp.path().join("sums"),
            format!("{}  ../outside\n", "0".repeat(64)),
        )
        .unwrap();
        assert!(verify_manifest(tmp.path(), "sums").is_err());
    }
}
