//! Install this running Venus build from its finite normal-entry RAM capsule.
//! The current physical boot is read only for a verified rollback backup; it
//! is never used as the source of the new recovery. Caller owns the global
//! installer lock and supplies freshly reconciled boot evidence for both calls.
use crate::venus_avb::CurrentDevice;
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use skyward_bootimg::capsule::{self, Manifest};
use std::os::{
    fd::{AsRawFd, FromRawFd},
    unix::{
        ffi::OsStrExt,
        fs::{FileExt, FileTypeExt, MetadataExt, OpenOptionsExt},
    },
};
use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

const CHUNK: usize = 256 * 1024;
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    rdev: u64,
    bytes: u64,
}
fn identity(file: &File, test: bool) -> Result<Identity> {
    let m = file.metadata()?;
    let bytes = if test {
        ensure!(m.is_file(), "test partition must be regular");
        m.len()
    } else {
        ensure!(
            m.file_type().is_block_device(),
            "boot target is not a block device"
        );
        let mut size = 0u64;
        #[cfg(target_os = "android")]
        let request = 0x8008_1272_u32 as libc::c_int;
        #[cfg(not(target_os = "android"))]
        let request = 0x8008_1272_u64 as libc::c_ulong;
        ensure!(
            unsafe { libc::ioctl(file.as_raw_fd(), request, &mut size) } == 0,
            "cannot read boot block size"
        );
        size
    };
    Ok(Identity {
        dev: m.dev(),
        ino: m.ino(),
        rdev: m.rdev(),
        bytes,
    })
}
fn hash_file(file: &File, bytes: u64, mut progress: impl FnMut(u64)) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; CHUNK];
    let mut done = 0;
    while done < bytes {
        let n = (bytes - done).min(CHUNK as u64) as usize;
        file.read_exact_at(&mut buffer[..n], done)?;
        hasher.update(&buffer[..n]);
        done += n as u64;
        progress(done);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn read_ram(path: &Path, maximum: usize, proc_exe: bool) -> Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    options.custom_flags(libc::O_CLOEXEC | if proc_exe { 0 } else { libc::O_NOFOLLOW });
    let file = options
        .open(path)
        .with_context(|| format!("open immutable RAM input {}", path.display()))?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == 0
            && meta.mode() & 0o022 == 0
            && meta.len() <= maximum as u64,
        "unsafe RAM capsule input {}",
        path.display()
    );
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= maximum && bytes.len() as u64 == meta.len(),
        "RAM capsule size changed"
    );
    Ok(bytes)
}
fn live(device: &CurrentDevice) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "current Recovery installation requires root"
    );
    ensure!(
        fs::read_to_string(BOOT_ID)?.trim() == device.boot_id(),
        "boot changed since evidence collection"
    );
    ensure!(
        skyward_block::slot_suffix()?.as_deref() == Some(device.slot_suffix()),
        "booted slot disagrees with installation evidence"
    );
    ensure!(
        skyward_vab::snapshot_state()? == skyward_vab::SnapshotState::None,
        "OTA snapshot state must be confirmed clear before installing boot"
    );
    let marker = fs::read_to_string("/skyward/venus-recovery.txt")?;
    ensure!(
        marker.starts_with("venus-recovery-rw-v1\n"),
        "not the marked Venus Recovery"
    );
    Ok(())
}

fn c_name(path: &Path) -> Result<CString> {
    let name = path.file_name().context("backup has no file name")?;
    CString::new(name.as_bytes()).context("backup name contains NUL")
}

/// Open every component relative to an owned directory descriptor, refusing
/// symlinks, '..' and missing parents. The wrapper creates its backup directory.
fn open_backup_parent(path: &Path) -> Result<File> {
    ensure!(path.is_absolute(), "backup path must be absolute");
    let normalized: PathBuf = path.components().collect();
    ensure!(
        normalized.as_os_str() == path.as_os_str(),
        "backup path must not contain redundant separators or dot components"
    );
    let parent = path.parent().context("backup path has no parent")?;
    c_name(path)?;
    let mut dir = File::open("/").context("open filesystem root")?;
    for part in parent.components() {
        let Component::Normal(name) = part else {
            ensure!(
                matches!(part, Component::RootDir),
                "backup path contains an unsafe component"
            );
            continue;
        };
        let name = CString::new(name.as_bytes()).context("backup path contains NUL")?;
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error())
                .context("open backup parent without following symlinks");
        }
        dir = unsafe { File::from_raw_fd(fd) };
    }
    Ok(dir)
}

fn check_backup_absent(parent: &File, path: &Path) -> Result<()> {
    let name = c_name(path)?;
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        bail!("backup already exists; no existing file or symlink will be replaced");
    }
    let error = io::Error::last_os_error();
    ensure!(
        error.kind() == io::ErrorKind::NotFound,
        "cannot establish that backup is absent: {error}"
    );
    Ok(())
}
fn durable_filesystem(file: &File, regular_test_file: bool) -> Result<()> {
    if regular_test_file {
        return Ok(());
    }
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error()).context("identify backup filesystem");
    }
    let kind = stat.f_type as u64;
    ensure!(
        matches!(kind, 0xef53 | 0xf2f5_2010),
        "backup must reside on durable ext4/f2fs, not recovery RAM or an unverified filesystem"
    );
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub partition: String,
    pub bytes: u64,
    pub previous_sha256: String,
    pub installed_sha256: String,
    pub running_build_sha256: String,
    pub backup_path: PathBuf,
    pub already_installed: bool,
}
pub struct ValidatedPlan {
    device: CurrentDevice,
    target: PathBuf,
    file: File,
    identity: Identity,
    image: Vec<u8>,
    summary: Summary,
    test: bool,
}
impl ValidatedPlan {
    pub fn summary(&self) -> &Summary {
        &self.summary
    }
}

pub fn prepare(device: &CurrentDevice, backup_path: &Path) -> Result<ValidatedPlan> {
    live(device)?;
    let manifest_bytes = read_ram(
        Path::new("/").join(capsule::MANIFEST_PATH).as_path(),
        capsule::MAX_MANIFEST,
        false,
    )?;
    let manifest: Manifest = capsule::manifest(&manifest_bytes)?;
    let template = read_ram(
        Path::new("/").join(capsule::TEMPLATE_PATH).as_path(),
        capsule::MAX_TEMPLATE,
        false,
    )?;
    let executable = read_ram(Path::new("/proc/self/exe"), 64 * 1024 * 1024, true)?;
    let config = read_ram(Path::new("/skyward/device.toml"), 1024 * 1024, false)?;
    ensure!(
        capsule::sha256(&config) == manifest.config_sha256,
        "running device configuration differs from capsule"
    );
    let image = capsule::rebuild(&template, &manifest_bytes, &executable, |p| {
        read_ram(
            Path::new("/").join(&p.path).as_path(),
            p.size as usize,
            false,
        )
    })?;
    live(device)?;
    let target = Path::new(skyward_block::BY_NAME).join(format!("boot{}", device.slot_suffix()));
    prepare_at(
        device,
        backup_path,
        &target,
        image,
        manifest.recovery_sha256,
        false,
    )
}
fn prepare_at(
    device: &CurrentDevice,
    backup_path: &Path,
    target: &Path,
    image: Vec<u8>,
    build: String,
    test: bool,
) -> Result<ValidatedPlan> {
    let parent = open_backup_parent(backup_path)?;
    check_backup_absent(&parent, backup_path)?;
    durable_filesystem(&parent, test)?;
    // Resolve only the trusted by-name alias, then pin its exact block-device fd.
    let canonical = fs::canonicalize(target)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&canonical)?;
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "boot partition already owned"
    );
    let identity = identity(&file, test)?;
    ensure!(
        self::identity(&File::open(target)?, test)? == identity,
        "boot by-name alias changed while opening"
    );
    ensure!(
        identity.bytes == image.len() as u64
            && (test || identity.bytes == capsule::PARTITION_SIZE as u64),
        "complete boot partition size mismatch"
    );
    skyward_block::drop_cache(&file)?;
    let previous = hash_file(&file, identity.bytes, |_| {})?;
    let installed = capsule::sha256(&image);
    let summary = Summary {
        partition: format!("boot{}", device.slot_suffix()),
        bytes: identity.bytes,
        already_installed: previous == installed,
        previous_sha256: previous,
        installed_sha256: installed,
        running_build_sha256: build,
        backup_path: backup_path.into(),
    };
    Ok(ValidatedPlan {
        device: device.clone(),
        target: target.into(),
        file,
        identity,
        image,
        summary,
        test,
    })
}

#[derive(Debug)]
pub struct Outcome {
    pub summary: Summary,
    pub changed: bool,
}
#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub backup_path: PathBuf,
    pub backup_verified: bool,
    /// If true, the partition may be partially written. Preserve the backup;
    /// do not turn this error into a success or an automatic reboot.
    pub write_started: bool,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (backup={}, verified={}, boot_write_started={})",
            self.message,
            self.backup_path.display(),
            self.backup_verified,
            self.write_started
        )
    }
}
impl std::error::Error for Failure {}

pub fn execute(
    plan: ValidatedPlan,
    fresh: &CurrentDevice,
    progress: &mut dyn FnMut(u64, u64),
) -> std::result::Result<Outcome, Failure> {
    execute_with(plan, fresh, progress, live)
}
fn execute_with(
    plan: ValidatedPlan,
    fresh: &CurrentDevice,
    progress: &mut dyn FnMut(u64, u64),
    recheck: impl Fn(&CurrentDevice) -> Result<()>,
) -> std::result::Result<Outcome, Failure> {
    let mut backup_verified = false;
    let mut write_started = false;
    let result = (|| -> Result<()> {
        ensure!(
            &plan.device == fresh,
            "installation evidence changed since preparation"
        );
        recheck(fresh)?;
        ensure!(
            identity(&plan.file, plan.test)? == plan.identity,
            "opened boot identity changed"
        );
        let named = File::open(&plan.target)?;
        ensure!(
            identity(&named, plan.test)? == plan.identity,
            "boot by-name alias changed"
        );
        skyward_block::drop_cache(&plan.file)?;
        ensure!(
            hash_file(&plan.file, plan.identity.bytes, |_| {})? == plan.summary.previous_sha256,
            "boot changed after preparation"
        );
        let parent = open_backup_parent(&plan.summary.backup_path)?;
        durable_filesystem(&parent, plan.test)?;
        let name = c_name(&plan.summary.backup_path)?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error()).context("create exclusive boot backup");
        }
        let backup = unsafe { File::from_raw_fd(fd) };
        let mut buffer = vec![0; CHUNK];
        let mut offset = 0;
        let total = plan.identity.bytes * 3;
        while offset < plan.identity.bytes {
            let n = (plan.identity.bytes - offset).min(CHUNK as u64) as usize;
            plan.file.read_exact_at(&mut buffer[..n], offset)?;
            backup.write_all_at(&buffer[..n], offset)?;
            offset += n as u64;
            progress(offset, total);
        }
        backup.sync_all()?;
        parent.sync_all()?;
        skyward_block::drop_cache(&backup)?;
        ensure!(
            hash_file(&backup, plan.identity.bytes, |_| {})? == plan.summary.previous_sha256,
            "complete boot backup readback mismatch"
        );
        backup_verified = false;
        backup_still_named(&backup, &parent, &plan)?;
        backup_verified = true;
        recheck(fresh)?;
        ensure!(
            identity(&plan.file, plan.test)? == plan.identity
                && hash_file(&plan.file, plan.identity.bytes, |_| {})?
                    == plan.summary.previous_sha256,
            "boot changed while preparing its backup"
        );
        ensure!(
            identity(&File::open(&plan.target)?, plan.test)? == plan.identity,
            "boot by-name alias changed before writing"
        );
        backup_verified = false;
        backup_still_named(&backup, &parent, &plan)?;
        backup_verified = true;
        if !plan.summary.already_installed {
            write_started = true;
            for (chunk, data) in plan.image.chunks(CHUNK).enumerate() {
                let offset = (chunk * CHUNK) as u64;
                plan.file.write_all_at(data, offset)?;
                progress(plan.identity.bytes + offset + data.len() as u64, total);
            }
            plan.file.sync_all()?;
        }
        skyward_block::drop_cache(&plan.file)?;
        let got = hash_file(&plan.file, plan.identity.bytes, |n| {
            progress(2 * plan.identity.bytes + n, total)
        })?;
        ensure!(
            got == plan.summary.installed_sha256,
            "installed complete boot readback differs"
        );
        ensure!(
            identity(&plan.file, plan.test)? == plan.identity,
            "installed boot identity changed"
        );
        backup_verified = false;
        backup_still_named(&backup, &parent, &plan)?;
        backup_verified = true;
        Ok(())
    })();
    match result {
        Ok(()) => Ok(Outcome {
            changed: !plan.summary.already_installed,
            summary: plan.summary,
        }),
        Err(e) => Err(Failure {
            message: format!("{e:#}"),
            backup_path: plan.summary.backup_path,
            backup_verified,
            write_started,
        }),
    }
}

fn backup_still_named(file: &File, parent: &File, plan: &ValidatedPlan) -> Result<()> {
    let current_parent = open_backup_parent(&plan.summary.backup_path)?;
    let held = parent.metadata()?;
    let current = current_parent.metadata()?;
    ensure!(
        held.dev() == current.dev() && held.ino() == current.ino(),
        "boot backup directory was replaced"
    );
    let name = c_name(&plan.summary.backup_path)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("reopen verified boot backup");
    }
    let named = unsafe { File::from_raw_fd(fd) };
    let a = file.metadata()?;
    let b = named.metadata()?;
    ensure!(
        a.is_file()
            && a.nlink() == 1
            && a.dev() == b.dev()
            && a.ino() == b.ino()
            && a.len() == plan.identity.bytes
            && b.len() == a.len(),
        "verified boot backup was replaced or resized"
    );
    skyward_block::drop_cache(file)?;
    ensure!(
        hash_file(file, plan.identity.bytes, |_| {})? == plan.summary.previous_sha256,
        "verified boot backup contents changed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn device(slot: &str) -> CurrentDevice {
        CurrentDevice::new(
            "venus",
            "00000000-0000-0000-0000-000000000001",
            slot,
            "",
            "orange",
            "",
        )
        .unwrap()
    }
    fn plan(dir: &Path) -> ValidatedPlan {
        fs::write(dir.join("boot_a"), vec![0x55; 4096]).unwrap();
        prepare_at(
            &device("_a"),
            &dir.join("backup.img"),
            &dir.join("boot_a"),
            vec![0x33; 4096],
            "build".into(),
            true,
        )
        .unwrap()
    }
    #[test]
    fn backs_up_whole_old_boot_then_writes_and_reads_back_exact_new_image() {
        let dir = tempfile::tempdir().unwrap();
        let p = plan(dir.path());
        let out = execute_with(p, &device("_a"), &mut |_, _| {}, |_| Ok(())).unwrap();
        assert!(out.changed);
        assert_eq!(
            fs::read(dir.path().join("backup.img")).unwrap(),
            vec![0x55; 4096]
        );
        assert_eq!(
            fs::read(dir.path().join("boot_a")).unwrap(),
            vec![0x33; 4096]
        );
    }
    #[test]
    fn identity_or_preimage_change_stops_before_backup_or_write() {
        let dir = tempfile::tempdir().unwrap();
        let p = plan(dir.path());
        let error = execute_with(p, &device("_b"), &mut |_, _| {}, |_| Ok(())).unwrap_err();
        assert!(!error.write_started && !error.backup_verified);
        let p = plan(dir.path());
        fs::write(dir.path().join("boot_a"), vec![0x99; 4096]).unwrap();
        let error = execute_with(p, &device("_a"), &mut |_, _| {}, |_| Ok(())).unwrap_err();
        assert!(!error.write_started && !dir.path().join("backup.img").exists());
    }
    #[test]
    fn existing_backup_and_stale_evidence_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let p = plan(dir.path());
        fs::write(dir.path().join("backup.img"), b"keep me").unwrap();
        let error = execute_with(p, &device("_a"), &mut |_, _| {}, |_| Ok(())).unwrap_err();
        assert!(!error.write_started);
        assert_eq!(fs::read(dir.path().join("backup.img")).unwrap(), b"keep me");
        assert_eq!(
            fs::read(dir.path().join("boot_a")).unwrap(),
            vec![0x55; 4096]
        );
    }
    #[test]
    fn symlink_parent_and_dangling_backup_are_refused() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let p = plan(dir.path());
        drop(p);
        symlink("missing", dir.path().join("backup.img")).unwrap();
        assert!(prepare_at(
            &device("_a"),
            &dir.path().join("backup.img"),
            &dir.path().join("boot_a"),
            vec![0; 4096],
            "build".into(),
            true
        )
        .is_err());
        symlink(dir.path(), dir.path().join("alias")).unwrap();
        assert!(open_backup_parent(&dir.path().join("alias/new.img")).is_err());
    }
    #[test]
    fn backup_substitution_before_write_preserves_original_boot() {
        let dir = tempfile::tempdir().unwrap();
        let p = plan(dir.path());
        let calls = std::cell::Cell::new(0);
        let error = execute_with(p, &device("_a"), &mut |_, _| {}, |_| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                fs::write(dir.path().join("backup.img"), vec![0x77; 4096])?;
            }
            Ok(())
        })
        .unwrap_err();
        assert!(!error.write_started && !error.backup_verified);
        assert_eq!(
            fs::read(dir.path().join("boot_a")).unwrap(),
            vec![0x55; 4096]
        );
    }
    #[test]
    fn post_write_corruption_reports_possible_mutation_and_keeps_backup() {
        let dir = tempfile::tempdir().unwrap();
        let p = plan(dir.path());
        let error = execute_with(
            p,
            &device("_a"),
            &mut |done, _| {
                if done == 8192 {
                    fs::write(dir.path().join("boot_a"), vec![0x88; 4096]).unwrap();
                }
            },
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(error.write_started && error.backup_verified);
        assert_eq!(
            fs::read(dir.path().join("backup.img")).unwrap(),
            vec![0x55; 4096]
        );
    }
}
