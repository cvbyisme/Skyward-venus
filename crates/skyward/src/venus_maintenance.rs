//! Explicit current-slot maintenance, shared by the UI and unattended CLI.
//! UI owns the process lease from startup; a worker clones that same description.
//! A standalone CLI takes the same lease and therefore cannot cross a live UI.
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use skyward_install::venus_avb::CurrentDevice;

fn reconcile(name: &str, values: &[String], allowed: &[&str]) -> Result<String> {
    let mut selected = "";
    for value in values.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        ensure!(
            allowed.contains(&value),
            "unsupported {name} evidence: {value}"
        );
        ensure!(
            selected.is_empty() || selected == value,
            "contradictory {name} evidence"
        );
        selected = value;
    }
    Ok(selected.to_owned())
}

fn kernel_values(text: &str, key: &str, bootconfig: bool) -> Result<Vec<String>> {
    let mut values = Vec::new();
    if bootconfig {
        for line in text.lines() {
            if let Some((name, value)) = line.split_once('=')
                && name.trim() == key
            {
                let value = value.trim().trim_end_matches(';').trim();
                let value = if value.starts_with('"') {
                    value
                        .strip_prefix('"')
                        .and_then(|s| s.strip_suffix('"'))
                        .context("malformed quoted boot evidence")?
                } else {
                    value
                };
                ensure!(
                    !value.contains(['"', ',', '\n', ' ']),
                    "ambiguous boot evidence"
                );
                values.push(value.into());
            }
        }
    } else {
        for token in text.split_whitespace() {
            if let Some(value) = token.strip_prefix(key).and_then(|s| s.strip_prefix('=')) {
                ensure!(!value.contains(['"', ',']), "ambiguous cmdline evidence");
                values.push(value.into());
            }
        }
    }
    Ok(values)
}

fn context_from(
    boot_id: &str,
    device: &[String],
    slot: &[String],
    state: &[String],
    verified: &[String],
    locked: &[String],
) -> Result<CurrentDevice> {
    CurrentDevice::new(
        &reconcile("device", device, &["venus"])?,
        boot_id.trim(),
        &reconcile("slot", slot, &["_a", "_b"])?,
        &reconcile("device lock", state, &["unlocked"])?,
        &reconcile("verified boot", verified, &["orange"])?,
        &reconcile("flash lock", locked, &["0"])?,
    )
}

fn review_token(
    context: &CurrentDevice,
    action: &str,
    before: &str,
    candidate: &str,
    bytes: u64,
) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            format!(
                "venus-maintenance-v1\n{context:?}\n{action}\n{before}\n{candidate}\n{bytes}\n"
            )
            .as_bytes()
        )
    )
}

fn process_identity(stat: &str) -> Result<(u32, u64)> {
    let tail = stat.rsplit_once(')').context("malformed process stat")?.1;
    let fields: Vec<_> = tail.split_whitespace().collect();
    ensure!(fields.len() >= 20, "short process stat");
    Ok((fields[1].parse()?, fields[19].parse()?))
}
fn actual_lock_owner(locks: &str, major: u64, minor: u64, inode: u64) -> Result<Option<u32>> {
    let mut found = None;
    for line in locks.lines() {
        let f: Vec<_> = line.split_whitespace().collect();
        if f.len() != 8 || f[1..4] != ["FLOCK", "ADVISORY", "WRITE"] || f[6..] != ["0", "EOF"] {
            continue;
        }
        let d: Vec<_> = f[5].split(':').collect();
        if d.len() != 3 {
            continue;
        }
        if u64::from_str_radix(d[0], 16).ok() == Some(major)
            && u64::from_str_radix(d[1], 16).ok() == Some(minor)
            && d[2].parse::<u64>().ok() == Some(inode)
        {
            ensure!(found.is_none(), "ambiguous maintenance lock owner");
            found = Some(f[4].parse()?);
        }
    }
    Ok(found)
}

#[cfg(target_os = "android")]
mod runtime {
    use super::*;
    use skyward_install::{venus_avb, venus_capsule};
    use skyward_ui::app::VenusMaintenance as Action;
    use std::{
        fs::{self, File, OpenOptions},
        os::{
            fd::AsRawFd,
            unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt},
        },
        path::Path,
        sync::{
            mpsc::{self, Receiver, Sender},
            Mutex,
        },
        thread,
        time::{Duration, Instant},
    };
    const LOCK: &str = "/skyward/run/locks/maintenance.lock";
    const LEGACY_LOCK: &str = "/skyward/run/locks/maintenance-legacy.lock";
    static PROCESS_LEASE: Mutex<Option<File>> = Mutex::new(None);
    const DATA_LOCK: &str = "/skyward/run/locks/data.lock";
    const BACKUPS: &str = "/data/media/0/SkywardRecoveryMaintenance";

    fn lock(path: &str) -> Result<File> {
        ensure!(
            crate::venus_rw::enabled() && unsafe { libc::geteuid() } == 0,
            "requires the marked Venus Recovery and root"
        );
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?;
        let m = file.metadata()?;
        ensure!(
            m.is_file() && m.uid() == 0 && m.nlink() == 1 && m.mode() & 0o077 == 0,
            "unsafe maintenance lock"
        );
        ensure!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "another UI, install or maintenance operation owns {path}; stop the UI before standalone maintenance");
        Ok(file)
    }
    pub fn lease() -> Result<File> {
        let mut held = PROCESS_LEASE
            .lock()
            .map_err(|_| anyhow::anyhow!("maintenance lease poisoned"))?;
        if held.is_none() {
            let file = lock(LOCK)?;
            let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
            let (_, start) = process_identity(&fs::read_to_string("/proc/self/stat")?)?;
            let receipt = format!(
                "venus-maintenance-v1\n{}\n{}\n{start}\n",
                boot.trim(),
                std::process::id()
            );
            file.set_len(0)?;
            file.write_all_at(receipt.as_bytes(), 0)?;
            *held = Some(file);
        }
        held.as_ref()
            .context("lease missing")?
            .try_clone()
            .context("clone owned maintenance lease")
    }
    fn owned_by_ancestor() -> Result<bool> {
        let file = File::open(LOCK)?;
        let m = file.metadata()?;
        ensure!(
            m.is_file() && m.uid() == 0 && m.nlink() == 1 && m.mode() & 0o077 == 0 && m.len() < 256,
            "unsafe lease receipt"
        );
        let owner = || {
            actual_lock_owner(
                &fs::read_to_string("/proc/locks")?,
                libc::major(m.dev()) as u64,
                libc::minor(m.dev()) as u64,
                m.ino(),
            )
        };
        let Some(pid) = owner()? else {
            return Ok(false);
        };
        let receipt = fs::read_to_string(LOCK)?;
        let lines: Vec<_> = receipt.lines().collect();
        ensure!(
            lines.len() == 4
                && lines[0] == "venus-maintenance-v1"
                && lines[1] == fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim()
                && lines[2].parse::<u32>()? == pid,
            "lease owner boot/PID receipt mismatch"
        );
        let (_, start) = process_identity(&fs::read_to_string(format!("/proc/{pid}/stat"))?)?;
        ensure!(
            lines[3].parse::<u64>()? == start,
            "lease owner PID was reused"
        );
        let ours = fs::metadata("/proc/self/exe")?;
        let theirs = fs::metadata(format!("/proc/{pid}/exe"))?;
        ensure!(
            ours.dev() == theirs.dev() && ours.ino() == theirs.ino(),
            "lease owner is not this Recovery executable"
        );
        let (mut parent, _) = process_identity(&fs::read_to_string("/proc/self/stat")?)?;
        for _ in 0..128 {
            if parent == pid {
                ensure!(
                    owner()? == Some(pid)
                        && process_identity(&fs::read_to_string(format!("/proc/{pid}/stat"))?)?.1
                            == start,
                    "lease owner changed while checking ancestry"
                );
                return Ok(true);
            }
            if parent <= 1 {
                break;
            }
            parent = process_identity(&fs::read_to_string(format!("/proc/{parent}/stat"))?)?.0;
        }
        Ok(false)
    }
    /// A zip's real child may reuse its UI installer's operation boundary.
    /// This additionally holds the legacy gate, so maintenance cannot overlap
    /// an avbctl process that is still running. No fd loses CLOEXEC.
    pub fn legacy_lease() -> Result<(Option<File>, File)> {
        let gate = lock(LEGACY_LOCK)?;
        if owned_by_ancestor().unwrap_or(false) {
            Ok((None, gate))
        } else {
            Ok((Some(lease()?), gate))
        }
    }

    fn current() -> Result<CurrentDevice> {
        let cmdline = fs::read_to_string("/proc/cmdline")?;
        let bootconfig = match fs::read_to_string("/proc/bootconfig") {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        let evidence = |key: &str, prop: &str| -> Result<Vec<String>> {
            let mut v = kernel_values(&cmdline, key, false)?;
            v.extend(kernel_values(&bootconfig, key, true)?);
            if let Some(s) = skyward_sys::prop(prop) {
                v.push(s);
            }
            Ok(v)
        };
        let cfg = skyward_config::DeviceConfig::load(Path::new("/skyward/device.toml"))?;
        let mut device = evidence("androidboot.product.device", "ro.product.device")?;
        device.push(cfg.device.codename);
        let slot = evidence("androidboot.slot_suffix", "ro.boot.slot_suffix")?;
        ensure!(
            skyward_block::slot_suffix()?.is_some(),
            "kernel supplies no active slot"
        );
        context_from(
            &fs::read_to_string("/proc/sys/kernel/random/boot_id")?,
            &device,
            &slot,
            &evidence(
                "androidboot.vbmeta.device_state",
                "ro.boot.vbmeta.device_state",
            )?,
            &evidence("androidboot.verifiedbootstate", "ro.boot.verifiedbootstate")?,
            &evidence("androidboot.flash.locked", "ro.boot.flash.locked")?,
        )
    }
    fn quiet_usb() -> Result<()> {
        ensure!(
            skyward_sys::prop("sys.usb.state").as_deref() == Some("adb")
                && skyward_sys::prop("sys.usb.config").as_deref() == Some("adb")
                && skyward_sys::prop("sys.usb.ffs.mtp.ready").as_deref() != Some("1"),
            "maintenance requires adb alone; turn off MTP, sideload and fastboot first"
        );
        ensure!(
            matches!(
                skyward_sys::prop("init.svc.fastbootd").as_deref(),
                None | Some("stopped")
            ),
            "fastbootd must be stopped"
        );
        Ok(())
    }
    fn data_ready() -> Result<()> {
        crate::venus_rw::verify()?;
        ensure!(
            skyward_crypto::fscrypt::unlocked(Path::new("/data/media/0"))?,
            "unlock owner storage before maintenance"
        );
        Ok(())
    }
    fn backup_directory(create: bool) -> Result<bool> {
        match fs::symlink_metadata(BACKUPS) {
            Ok(m) => {
                ensure!(
                    m.is_dir()
                        && !m.file_type().is_symlink()
                        && m.uid() == 0
                        && m.mode() & 0o077 == 0,
                    "maintenance backup directory must be a root-owned private real directory"
                );
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !create => Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::DirBuilder::new().mode(0o700).create(BACKUPS)?;
                File::open(BACKUPS)?.sync_all()?;
                File::open("/data/media/0")?.sync_all()?;
                backup_directory(false)
            }
            Err(e) => Err(e.into()),
        }
    }
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
    enum Backend {
        Avb(venus_avb::ValidatedPlan),
        Recovery(venus_capsule::ValidatedPlan),
    }
    struct Plan {
        context: CurrentDevice,
        backend: Backend,
        summary: String,
        token: String,
        preview_only: bool,
        _lease: File,
        _data: File,
        _legacy: File,
        _storage: File,
    }
    fn prepare(action: Action, lease: File, readonly: bool) -> Result<Plan> {
        skyward_install::task_scope::ensure_quiet()?;
        let legacy = lock(LEGACY_LOCK)?;
        let data = lock(DATA_LOCK)?;
        quiet_usb()?;
        data_ready()?;
        if let Some(battery) = skyward_info::battery_now()? {
            ensure!(
                skyward_install::battery_refusal(&battery).is_none(),
                "charge the battery before partition maintenance"
            );
        }
        let context = current()?;
        let needs_backup = matches!(action, Action::InstallRecovery);
        let parent_exists = if needs_backup { backup_directory(!readonly)? } else { false };
        let nonce = fs::read_to_string("/proc/sys/kernel/random/uuid")?;
        ensure!(
            nonce.trim().len() == 36
                && nonce
                    .trim()
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() || c == b'-'),
            "invalid backup nonce"
        );
        let partition = match action {
            Action::DisableAvb => "vbmeta",
            Action::InstallRecovery => "boot",
        };
        let name = format!("{partition}{}-{}.img", context.slot_suffix(), nonce.trim());
        let intended = Path::new(BACKUPS).join(&name);
        // Read-only CLI planning creates no persistent directory/file. If the
        // dedicated directory is new, inspect the same durable parent filesystem;
        // this preview is never executable and --execute prepares again there.
        let selected = if parent_exists {
            intended.clone()
        } else {
            Path::new("/data/media/0").join(&name)
        };
        let (backend, mut summary, token) = match action {
            Action::DisableAvb => {
                let p = venus_avb::prepare_unbacked(&context)?;
                let summary = format!("目标: vbmeta{}\nflags: 0x{:08x} → 0x{:08x}\n写入后读回校验，不创建备份。",
                    p.slot_suffix(), p.header().flags, p.header().flags | 3);
                let token = review_token(
                    &context,
                    "disable-avb",
                    &hex(&p.original_sha256()),
                    &format!("flags={:08x}", p.header().flags | 3),
                    p.partition_bytes(),
                );
                (Backend::Avb(p), summary, token)
            }
            Action::InstallRecovery => {
                let p = venus_capsule::prepare(&context, &selected)?;
                let s = p.summary();
                let summary = format!("目标分区: {}\n大小: {} 字节\n原始 SHA-256: {}\n安装 SHA-256: {}\n当前构建 SHA-256: {}\n备份: {}\n安装当前运行构建，保留原始正常启动 init。",
                    s.partition, s.bytes, s.previous_sha256, s.installed_sha256, s.running_build_sha256, intended.display());
                let token = review_token(
                    &context,
                    "install-recovery",
                    &s.previous_sha256,
                    &s.installed_sha256,
                    s.bytes,
                );
                (Backend::Recovery(p), summary, token)
            }
        };
        if needs_backup && !parent_exists {
            summary.push_str("\nparent_will_be_created=true (只读计划未创建目录)");
        }
        Ok(Plan {
            context,
            backend,
            summary,
            token,
            preview_only: readonly,
            _lease: lease,
            _data: data,
            _legacy: legacy,
            _storage: File::open("/data/media/0")?,
        })
    }
    pub struct Completion {
        pub success: bool,
        pub detail: String,
        pub write_started: bool,
    }
    fn execute(plan: Plan, progress: &mut dyn FnMut(u64, u64)) -> Completion {
        let checked = (|| -> Result<CurrentDevice> {
            ensure!(!plan.preview_only, "read-only preview cannot be executed");
            skyward_install::task_scope::ensure_quiet()?;
            quiet_usb()?;
            data_ready()?;
            let fresh = current()?;
            ensure!(
                fresh == plan.context,
                "device/boot/slot/unlock evidence changed"
            );
            Ok(fresh)
        })();
        let fresh = match checked {
            Ok(c) => c,
            Err(e) => {
                return Completion {
                    success: false,
                    detail: format!("{e:#}"),
                    write_started: false,
                }
            }
        };
        match plan.backend {
            Backend::Avb(p) => match venus_avb::execute(p, &fresh) {
                Ok(o) => Completion {
                    success: true,
                    write_started: o.changed,
                    detail: if o.changed {
                        "AVB 已禁用（verity + verification）。".into()
                    } else {
                        "AVB 已经是禁用状态。".into()
                    },
                },
                Err(e) => Completion {
                    success: false,
                    write_started: e.partition_may_have_changed,
                    detail: e.to_string(),
                },
            },
            Backend::Recovery(p) => match venus_capsule::execute(p, &fresh, progress) {
                Ok(o) => Completion {
                    success: true,
                    write_started: o.changed,
                    detail: format!(
                        "已验证 {} 完整读回。\nSHA-256: {}\n备份: {}",
                        o.summary.partition,
                        o.summary.installed_sha256,
                        o.summary.backup_path.display()
                    ),
                },
                Err(e) => Completion {
                    success: false,
                    write_started: e.write_started,
                    detail: e.to_string(),
                },
            },
        }
    }
    pub enum Event {
        Ready(String),
        Progress(u64, u64),
        Finished(Completion),
    }
    enum Command {
        Execute,
        Cancel,
    }
    pub struct Worker {
        events: Receiver<Event>,
        commands: Sender<Command>,
    }
    impl Worker {
        pub fn start(action: Action, lease: &File) -> Result<Self> {
            let lease = lease.try_clone()?;
            let (tx, events) = mpsc::channel();
            let (commands, rx) = mpsc::channel();
            thread::Builder::new()
                .name("venus-maintenance".into())
                .spawn(move || {
                    let plan = match prepare(action, lease, false) {
                        Ok(p) => p,
                        Err(e) => {
                            let _ = tx.send(Event::Finished(Completion {
                                success: false,
                                detail: format!("{e:#}"),
                                write_started: false,
                            }));
                            return;
                        }
                    };
                    if tx.send(Event::Ready(plan.summary.clone())).is_err() {
                        return;
                    }
                    if !matches!(rx.recv(), Ok(Command::Execute)) {
                        return;
                    }
                    let mut last = Instant::now() - Duration::from_secs(1);
                    let result = execute(plan, &mut |done, total| {
                        if done == total || last.elapsed() >= Duration::from_millis(100) {
                            let _ = tx.send(Event::Progress(done, total));
                            last = Instant::now();
                        }
                    });
                    let _ = tx.send(Event::Finished(result));
                })?;
            Ok(Self { events, commands })
        }
        pub fn events(&self) -> Vec<Event> {
            self.events.try_iter().collect()
        }
        pub fn execute(&self) -> Result<()> {
            self.commands
                .send(Command::Execute)
                .context("maintenance worker ended")
        }
    }
    impl Drop for Worker {
        fn drop(&mut self) {
            let _ = self.commands.send(Command::Cancel);
        }
    }
    pub fn cli(action: &str, commit: bool, expected: Option<&str>) -> Result<()> {
        let action = match action {
            "disable-avb" => Action::DisableAvb,
            "install-recovery" => Action::InstallRecovery,
            _ => anyhow::bail!("action must be disable-avb or install-recovery"),
        };
        ensure!(
            skyward_sys::prop("init.svc.venus_ui").as_deref() == Some("stopped"),
            "stop venus_ui before standalone maintenance; use its advanced menu while UI runs"
        );
        let p = prepare(action, lease()?, !commit)?;
        println!(
            "{}\nreadonly_plan={}\nreview_token={}",
            p.summary, !commit, p.token
        );
        if commit {
            ensure!(expected == Some(p.token.as_str()), "review token differs: device identity, old partition or candidate changed; rerun the read-only plan");
            let result = execute(p, &mut |done, total| {
                eprintln!("MAINTENANCE_PROGRESS {done}/{total}")
            });
            println!(
                "{}\nsuccess={} write_started={}",
                result.detail, result.success, result.write_started
            );
            ensure!(
                result.success,
                "maintenance failed; preserve any verified backup; no automatic reboot"
            );
        }
        Ok(())
    }
}
#[cfg(target_os = "android")]
pub use runtime::*;

#[cfg(test)]
mod tests {
    use super::*;
    fn vals(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn orange_kernel_evidence_is_positive_but_missing_or_conflicting_evidence_is_not() {
        let d = vals(&["venus"]);
        let slot = vals(&["_a", "_a"]);
        let boot = "00000000-0000-0000-0000-000000000001";
        assert!(context_from(boot, &d, &slot, &[], &vals(&["orange"]), &[]).is_ok());
        assert!(context_from(boot, &d, &slot, &[], &[], &vals(&["0"])).is_err());
        for values in [vals(&["orange", "green"]), vals(&["orange", "yellow"])] {
            assert!(context_from(boot, &d, &slot, &[], &values, &[]).is_err());
        }
        assert!(context_from(
            boot,
            &d,
            &vals(&["_a", "_b"]),
            &vals(&["unlocked"]),
            &[],
            &[]
        )
        .is_err());
        assert!(
            context_from(boot, &d, &slot, &vals(&["locked"]), &vals(&["orange"]), &[]).is_err()
        );
        assert!(context_from(boot, &d, &slot, &[], &vals(&["orange"]), &vals(&["1"])).is_err());
    }
    #[test]
    fn every_kernel_source_and_duplicate_is_retained_for_reconciliation() {
        let mut values = kernel_values(
            "androidboot.slot_suffix=_a androidboot.slot_suffix=_b",
            "androidboot.slot_suffix",
            false,
        )
        .unwrap();
        values.extend(
            kernel_values(
                "androidboot.slot_suffix = \"_a\"\n",
                "androidboot.slot_suffix",
                true,
            )
            .unwrap(),
        );
        assert_eq!(values, vals(&["_a", "_b", "_a"]));
        assert!(reconcile("slot", &values, &["_a", "_b"]).is_err());
        assert!(kernel_values(
            "androidboot.flash.locked = \"0\", \"1\"",
            "androidboot.flash.locked",
            true
        )
        .is_err());
    }
    #[test]
    fn review_token_binds_boot_slot_preimage_and_exact_candidate() {
        let a = CurrentDevice::new(
            "venus",
            "00000000-0000-0000-0000-000000000001",
            "_a",
            "",
            "orange",
            "",
        )
        .unwrap();
        let b = CurrentDevice::new(
            "venus",
            "00000000-0000-0000-0000-000000000002",
            "_a",
            "",
            "orange",
            "",
        )
        .unwrap();
        let token = review_token(&a, "install-recovery", "old", "new", 201326592);
        assert_ne!(
            token,
            review_token(&b, "install-recovery", "old", "new", 201326592)
        );
        assert_ne!(
            token,
            review_token(&a, "install-recovery", "old2", "new", 201326592)
        );
        assert_ne!(
            token,
            review_token(&a, "install-recovery", "old", "new2", 201326592)
        );
        assert_ne!(
            token,
            review_token(&a, "disable-avb", "old", "new", 201326592)
        );
    }
    #[test]
    fn ancestry_reuse_requires_the_exact_exclusive_kernel_lock_record() {
        let locks = "1: POSIX ADVISORY WRITE 99 08:03:42 0 EOF\n2: FLOCK ADVISORY WRITE 123 08:03:42 0 EOF\n3: -> FLOCK ADVISORY WRITE 456 08:03:42 0 EOF\n";
        assert_eq!(actual_lock_owner(locks, 8, 3, 42).unwrap(), Some(123));
        assert_eq!(actual_lock_owner(locks, 8, 3, 43).unwrap(), None);
        assert!(actual_lock_owner(
            &(locks.to_owned() + "4: FLOCK ADVISORY WRITE 124 08:03:42 0 EOF\n"),
            8,
            3,
            42
        )
        .is_err());
        let stat = format!("123 (a tricky ) name) S 12 {} 99", vec!["0"; 17].join(" "));
        assert_eq!(process_identity(&stat).unwrap(), (12, 99));
    }
}
