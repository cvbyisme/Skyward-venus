//! Guarded, current-slot Venus vbmeta flag update, independent of avbctl.
//!
//! Not exported until the device/UI wrapper is ready. The wrapper supplies
//! fresh boot evidence and holds the shared installer/flash-operation lock.
//! This module also locks the partition, binds a plan to its entire original
//! contents, and never substitutes a missing/unknown lock state for unlocked.
//!
//! Layout/bounds follow AOSP libavb's avb_vbmeta_image.h/.c (AVB 1.3):
//! https://android.googlesource.com/platform/external/avb/+/refs/heads/main/libavb/avb_vbmeta_image.h
//! This validates structure, not the authenticity of an AVB signature. Changing
//! flags intentionally changes the signed header; no signing key is invented.

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

const HEADER_SIZE: usize = 256;
const FLAGS_OFFSET: usize = 120;
const DISABLE_BITS: u32 = 3;
const MAX_PARTITION_BYTES: u64 = 16 * 1024 * 1024;
const BY_NAME: &str = "/dev/block/by-name";
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";

/// Evidence collected afresh by the wrapper from boot properties/cmdline.
/// Private fields prevent constructing a state without the positive checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentDevice {
    device: String,
    boot_id: String,
    slot_suffix: String,
    device_state: String,
    verified_boot_state: String,
    flash_locked: String,
}

impl CurrentDevice {
    /// All nonempty evidence must agree. The wrapper must also reject
    /// contradictions between properties and the kernel command line before
    /// choosing the values passed here. "0" alone is not positive unlock proof.
    pub fn new(
        device: &str,
        boot_id: &str,
        slot_suffix: &str,
        device_state: &str,
        verified_boot_state: &str,
        flash_locked: &str,
    ) -> Result<Self> {
        ensure!(device == "venus", "AVB operation is restricted to Venus");
        ensure!(
            matches!(slot_suffix, "_a" | "_b"),
            "missing or invalid current slot suffix"
        );
        ensure!(valid_boot_id(boot_id), "missing or malformed boot ID");
        ensure!(
            matches!(device_state, "" | "unlocked"),
            "bootloader state is locked or unrecognized"
        );
        ensure!(
            matches!(verified_boot_state, "" | "orange"),
            "verified-boot evidence is locked or contradictory"
        );
        ensure!(
            matches!(flash_locked, "" | "0"),
            "flash-lock evidence is locked or unrecognized"
        );
        ensure!(
            device_state == "unlocked" || verified_boot_state == "orange",
            "no positive bootloader-unlocked evidence"
        );
        Ok(Self {
            device: device.into(),
            boot_id: boot_id.into(),
            slot_suffix: slot_suffix.into(),
            device_state: device_state.into(),
            verified_boot_state: verified_boot_state.into(),
            flash_locked: flash_locked.into(),
        })
    }

    pub fn slot_suffix(&self) -> &str {
        &self.slot_suffix
    }
    pub fn boot_id(&self) -> &str {
        &self.boot_id
    }
}

fn valid_boot_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub flags: u32,
    pub vbmeta_bytes: u64,
    pub algorithm: u32,
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().expect("checked header field"))
}
fn be64(bytes: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(bytes[at..at + 8].try_into().expect("checked header field"))
}
fn span(offset: u64, size: u64, limit: u64, name: &str) -> Result<(u64, u64)> {
    let end = offset
        .checked_add(size)
        .context("AVB region integer overflow")?;
    ensure!(end <= limit, "{name} extends outside its AVB block");
    Ok((offset, end))
}
fn nonoverlap(a: (u64, u64), b: (u64, u64)) -> bool {
    a.0 == a.1 || b.0 == b.1 || a.1 <= b.0 || b.1 <= a.0
}

/// Validate the top-level header at byte zero and every region it references.
/// Embedded vbmeta footers and chained vbmeta partitions are not accepted.
pub fn validate_image(image: &[u8]) -> Result<Header> {
    ensure!(
        image.len() >= HEADER_SIZE,
        "vbmeta partition is shorter than its 256-byte header"
    );
    ensure!(
        image.len() as u64 <= MAX_PARTITION_BYTES,
        "vbmeta partition exceeds the bounded Venus limit"
    );
    ensure!(
        &image[..4] == b"AVB0",
        "top-level vbmeta magic is missing at byte zero"
    );
    ensure!(
        be32(image, 4) == 1 && be32(image, 8) <= 3,
        "unsupported required libavb version"
    );
    ensure!(image[175] == 0, "AVB release string is not NUL terminated");
    ensure!(
        image[176..256].iter().all(|b| *b == 0),
        "nonzero reserved AVB header bytes"
    );
    ensure!(be32(image, 124) < 32, "invalid rollback index location");
    let auth = be64(image, 12);
    let aux = be64(image, 20);
    ensure!(
        auth.is_multiple_of(64) && aux.is_multiple_of(64),
        "AVB block sizes are not multiples of 64"
    );
    let aux_start = 256_u64
        .checked_add(auth)
        .context("AVB authentication size overflow")?;
    let total = aux_start
        .checked_add(aux)
        .context("AVB auxiliary size overflow")?;
    ensure!(
        total <= image.len() as u64,
        "AVB blocks exceed the complete partition"
    );
    let hash = span(be64(image, 32), be64(image, 40), auth, "hash")?;
    let sig = span(be64(image, 48), be64(image, 56), auth, "signature")?;
    let key = span(be64(image, 64), be64(image, 72), aux, "public key")?;
    let keymeta = span(be64(image, 80), be64(image, 88), aux, "public key metadata")?;
    let desc = span(be64(image, 96), be64(image, 104), aux, "descriptors")?;
    ensure!(
        nonoverlap(hash, sig),
        "overlapping AVB authentication regions"
    );
    ensure!(
        nonoverlap(key, keymeta) && nonoverlap(key, desc) && nonoverlap(keymeta, desc),
        "overlapping AVB auxiliary regions"
    );
    let algorithm = be32(image, 28);
    let (hash_size, sig_size, key_size) = match algorithm {
        0 => (0, 0, 0),
        1 => (32, 256, 520),
        2 => (32, 512, 1032),
        3 => (32, 1024, 2056),
        4 => (64, 256, 520),
        5 => (64, 512, 1032),
        6 => (64, 1024, 2056),
        _ => bail!("unsupported AVB algorithm"),
    };
    ensure!(
        hash.1 - hash.0 == hash_size && sig.1 - sig.0 == sig_size && key.1 - key.0 == key_size,
        "AVB authentication sizes disagree with the algorithm"
    );
    if algorithm == 0 {
        ensure!(
            auth == 0,
            "unsigned vbmeta unexpectedly has an authentication block"
        );
    }
    ensure!(
        desc.0 % 8 == 0 && (desc.1 - desc.0) % 8 == 0,
        "unaligned descriptor region"
    );
    let mut pos = (aux_start + desc.0) as usize;
    let end = (aux_start + desc.1) as usize;
    while pos < end {
        ensure!(end - pos >= 16, "truncated AVB descriptor header");
        let following = be64(image, pos + 8);
        ensure!(following.is_multiple_of(8), "unaligned AVB descriptor payload");
        let next = (pos as u64)
            .checked_add(16)
            .and_then(|p| p.checked_add(following))
            .context("AVB descriptor size overflow")?;
        ensure!(
            next <= end as u64,
            "AVB descriptor exceeds the declared descriptor region"
        );
        pos = next as usize;
    }
    Ok(Header {
        flags: be32(image, FLAGS_OFFSET),
        vbmeta_bytes: total,
        algorithm,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    rdev: u64,
    size: u64,
}

fn identity(file: &File, regular_test_file: bool) -> Result<Identity> {
    let meta = file.metadata().context("stat opened vbmeta partition")?;
    let size = if regular_test_file {
        ensure!(meta.is_file(), "test source is not a regular file");
        meta.len()
    } else {
        ensure!(
            meta.file_type().is_block_device(),
            "vbmeta source is not a block device"
        );
        let mut bytes = 0_u64;
        // _IOR(0x12, 114, size_t), ARM64/Linux BLKGETSIZE64.
        #[cfg(target_os = "android")]
        let request = 0x8008_1272_u32 as libc::c_int;
        #[cfg(not(target_os = "android"))]
        let request = 0x8008_1272_u64 as libc::c_ulong;
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), request, &mut bytes) };
        if rc != 0 {
            return Err(io::Error::last_os_error()).context("BLKGETSIZE64 on opened vbmeta");
        }
        bytes
    };
    ensure!(
        (256..=MAX_PARTITION_BYTES).contains(&size),
        "invalid or oversized vbmeta partition"
    );
    Ok(Identity {
        dev: meta.dev(),
        ino: meta.ino(),
        rdev: meta.rdev(),
        size,
    })
}

fn lock(file: &File) -> Result<()> {
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err(io::Error::last_os_error()).context("vbmeta is busy; exclusive lock refused");
    }
    Ok(())
}
fn read_partition(file: &File, identity: Identity) -> Result<Vec<u8>> {
    let mut image =
        vec![0; usize::try_from(identity.size).context("partition length does not fit memory")?];
    file.read_exact_at(&mut image, 0)
        .context("read the complete vbmeta partition")?;
    Ok(image)
}
fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn check_boot(device: &CurrentDevice) -> Result<()> {
    ensure!(
        fs::read_to_string(BOOT_ID)
            .context("read current kernel boot ID")?
            .trim()
            == device.boot_id,
        "boot ID changed or the supplied evidence is stale"
    );
    Ok(())
}

/// A non-cloneable, single-use plan. It owns the locked file descriptor, so a
/// later by-name symlink replacement cannot redirect the actual write.
pub struct ValidatedPlan {
    device: CurrentDevice,
    by_name: PathBuf,
    partition: File,
    identity: Identity,
    original: Vec<u8>,
    original_sha256: [u8; 32],
    header: Header,
    backup: Option<PathBuf>,
    regular_test_file: bool,
}

impl ValidatedPlan {
    pub fn header(&self) -> Header {
        self.header
    }
    pub fn partition_bytes(&self) -> u64 {
        self.identity.size
    }
    pub fn original_sha256(&self) -> [u8; 32] {
        self.original_sha256
    }
    pub fn backup_path(&self) -> Option<&Path> {
        self.backup.as_deref()
    }
    pub fn slot_suffix(&self) -> &str {
        self.device.slot_suffix()
    }
}

pub fn prepare(device: &CurrentDevice, backup_path: &Path) -> Result<ValidatedPlan> {
    let path = Path::new(BY_NAME).join(format!("vbmeta{}", device.slot_suffix));
    prepare_at(device, &path, backup_path, false)
}

/// Prepare the fastboot-equivalent flag change without creating a backup.
pub fn prepare_unbacked(device: &CurrentDevice) -> Result<ValidatedPlan> {
    let path = Path::new(BY_NAME).join(format!("vbmeta{}", device.slot_suffix));
    prepare_at_optional(device, &path, None, false)
}

fn prepare_at(
    device: &CurrentDevice,
    path: &Path,
    backup: &Path,
    regular_test_file: bool,
) -> Result<ValidatedPlan> {
    prepare_at_optional(device, path, Some(backup), regular_test_file)
}

fn prepare_at_optional(
    device: &CurrentDevice,
    path: &Path,
    backup: Option<&Path>,
    regular_test_file: bool,
) -> Result<ValidatedPlan> {
    check_boot(device)?;
    ensure!(
        path.file_name().and_then(|s| s.to_str()) == Some(&format!("vbmeta{}", device.slot_suffix)),
        "not the current slot top-level vbmeta name"
    );
    let canonical = fs::canonicalize(path).context("resolve current-slot vbmeta")?;
    let partition = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&canonical)
        .context("open current-slot vbmeta without following another final symlink")?;
    lock(&partition)?;
    let id = identity(&partition, regular_test_file)?;
    let selected = File::open(path).context("recheck by-name vbmeta selection")?;
    ensure!(
        identity(&selected, regular_test_file)? == id,
        "vbmeta by-name selection changed while opening"
    );
    let original = read_partition(&partition, id)?;
    let header = validate_image(&original)?;
    if let Some(backup) = backup {
        let parent = open_backup_parent(backup)?;
        check_backup_absent(&parent, backup)?;
        durable_filesystem(&parent, regular_test_file)?;
    }
    Ok(ValidatedPlan {
        device: device.clone(),
        by_name: path.into(),
        partition,
        identity: id,
        original_sha256: digest(&original),
        original,
        header,
        backup: backup.map(Path::to_path_buf),
        regular_test_file,
    })
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupReceipt {
    pub path: PathBuf,
    pub bytes: u64,
    /// Expected complete original hash. Always revalidate before any later
    /// restore; this receipt does not make a filesystem file immutable.
    pub sha256: [u8; 32],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Revalidate,
    Backup,
    WriteFlags,
    SyncPartition,
    ReadBack,
}

/// An error after the first write attempt always reports possible mutation.
/// The backend never automatically overwrites the partition again to roll back.
#[derive(Debug)]
pub struct Failure {
    pub stage: Stage,
    pub partition_may_have_changed: bool,
    /// A completed, fsynced and read-back-verified backup, if one exists.
    pub backup: Option<BackupReceipt>,
    pub detail: String,
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AVB {:?}: {}; partition {}",
            self.stage,
            self.detail,
            if self.partition_may_have_changed {
                "may have changed; preserve the verified backup"
            } else {
                "was not written by this operation"
            }
        )
    }
}
impl std::error::Error for Failure {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// False means already disabled, with no partition write and no new backup.
    pub changed: bool,
    pub slot_suffix: String,
    pub old_flags: u32,
    pub new_flags: u32,
    pub partition_bytes: u64,
    pub original_sha256: [u8; 32],
    pub result_sha256: [u8; 32],
    pub backup: Option<BackupReceipt>,
}

fn revalidate(plan: &ValidatedPlan, current: &CurrentDevice) -> Result<()> {
    ensure!(
        current == &plan.device,
        "current device/slot/unlock evidence differs from the prepared plan"
    );
    check_boot(current)?;
    ensure!(
        identity(&plan.partition, plan.regular_test_file)? == plan.identity,
        "opened partition identity/size changed"
    );
    let named = File::open(&plan.by_name).context("reopen current vbmeta by-name selection")?;
    ensure!(
        identity(&named, plan.regular_test_file)? == plan.identity,
        "by-name now selects a different partition"
    );
    let fresh = read_partition(&plan.partition, plan.identity)?;
    ensure!(
        fresh == plan.original && digest(&fresh) == plan.original_sha256,
        "vbmeta contents changed after preparation"
    );
    ensure!(
        validate_image(&fresh)? == plan.header,
        "vbmeta header changed after preparation"
    );
    Ok(())
}

fn backup(plan: &ValidatedPlan, path: &Path, io: &mut dyn PartitionIo) -> Result<(File, File, BackupReceipt)> {
    let parent = open_backup_parent(path)?;
    durable_filesystem(&parent, plan.regular_test_file)?;
    let name = c_name(path)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error())
            .context("create exclusive original-partition backup");
    }
    let file = unsafe { File::from_raw_fd(fd) };
    // On every failure retain the file for diagnosis; never replace or silently
    // delete it. It is not reported as a completed backup until all checks pass.
    file.write_all_at(&plan.original, 0)
        .context("write complete original partition backup")?;
    io.sync_backup(&file, &parent)
        .context("fsync original partition backup and directory entry")?;
    let meta = file.metadata().context("stat completed backup")?;
    ensure!(
        meta.is_file() && meta.nlink() == 1 && meta.len() == plan.identity.size,
        "backup type/length/link count changed"
    );
    let mut verified = vec![0; plan.original.len()];
    file.read_exact_at(&mut verified, 0)
        .context("read back complete original backup")?;
    ensure!(
        verified == plan.original && digest(&verified) == plan.original_sha256,
        "backup read-back differs from original partition"
    );
    Ok((
        file,
        parent,
        BackupReceipt {
            path: path.to_path_buf(),
            bytes: plan.identity.size,
            sha256: plan.original_sha256,
        },
    ))
}

fn backup_still_named(file: &File, parent: &File, path: &Path, original: &[u8]) -> Result<()> {
    let current_parent = open_backup_parent(path)?;
    let held = parent.metadata()?;
    let current = current_parent.metadata()?;
    ensure!(
        held.dev() == current.dev() && held.ino() == current.ino(),
        "backup directory path was renamed or replaced"
    );
    let name = c_name(path)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error())
            .context("recheck backup name before partition write");
    }
    let named = unsafe { File::from_raw_fd(fd) };
    let a = file.metadata()?;
    let b = named.metadata()?;
    ensure!(
        a.nlink() == 1 && a.dev() == b.dev() && a.ino() == b.ino() && a.len() == b.len(),
        "verified backup was unlinked or replaced"
    );
    ensure!(
        a.len() == original.len() as u64,
        "verified backup length changed"
    );
    let mut bytes = vec![0; original.len()];
    file.read_exact_at(&mut bytes, 0)
        .context("recheck complete backup before/after partition update")?;
    ensure!(bytes == original, "verified backup contents changed");
    Ok(())
}

/// Consume the plan, after the wrapper has freshly collected CurrentDevice.
/// Only this function writes, and its sole partition write is four bytes at 120.
pub fn execute(
    plan: ValidatedPlan,
    current: &CurrentDevice,
) -> std::result::Result<Outcome, Failure> {
    execute_with(plan, current, &mut RealIo)
}

trait PartitionIo {
    fn sync_backup(&mut self, file: &File, parent: &File) -> io::Result<()> {
        file.sync_all()?;
        parent.sync_all()
    }
    /// Write the complete fastboot-style vbmeta image, not only its flags.
    fn write_image(&mut self, file: &File, image: &[u8]) -> io::Result<()>;
    fn sync(&mut self, file: &File) -> io::Result<()>;
    fn read_back(&mut self, file: &File, bytes: &mut [u8]) -> io::Result<()>;
}
struct RealIo;
impl PartitionIo for RealIo {
    fn write_image(&mut self, file: &File, image: &[u8]) -> io::Result<()> {
        file.write_all_at(image, 0)
    }
    fn sync(&mut self, file: &File) -> io::Result<()> {
        file.sync_all()
    }
    fn read_back(&mut self, file: &File, bytes: &mut [u8]) -> io::Result<()> {
        file.read_exact_at(bytes, 0)
    }
}

fn execute_with(
    plan: ValidatedPlan,
    current: &CurrentDevice,
    io: &mut dyn PartitionIo,
) -> std::result::Result<Outcome, Failure> {
    let failure = |stage, changed, backup, error: anyhow::Error| Failure {
        stage,
        partition_may_have_changed: changed,
        backup,
        detail: format!("{error:#}"),
    };
    revalidate(&plan, current).map_err(|e| failure(Stage::Revalidate, false, None, e))?;
    let new_flags = plan.header.flags | DISABLE_BITS;
    let mut outcome = Outcome {
        changed: false,
        slot_suffix: plan.device.slot_suffix.clone(),
        old_flags: plan.header.flags,
        new_flags,
        partition_bytes: plan.identity.size,
        original_sha256: plan.original_sha256,
        result_sha256: plan.original_sha256,
        backup: None,
    };
    if new_flags == plan.header.flags {
        return Ok(outcome);
    }
    let held = match plan.backup.as_deref() {
        Some(path) => Some(backup(&plan, path, io).map_err(|e| failure(Stage::Backup, false, None, e))?),
        None => None,
    };
    let receipt = held.as_ref().map(|(_, _, receipt)| receipt.clone());
    // Backup work may take time. Recheck the entire target, boot identity, and
    // name one final time before the first write, preserving the verified backup.
    revalidate(&plan, current).and_then(|()| {
        if let Some((backup_file, parent, _)) = &held {
            backup_still_named(backup_file, parent, plan.backup.as_ref().expect("held backup has a path"), &plan.original)
        } else {
            Ok(())
        }
    })
    .map_err(|e| failure(Stage::Revalidate, false, receipt.clone(), e))?;
    let mut image = plan.original.clone();
    image[FLAGS_OFFSET..FLAGS_OFFSET + 4].copy_from_slice(&new_flags.to_be_bytes());
    io.write_image(&plan.partition, &image)
        .map_err(|e| failure(Stage::WriteFlags, true, receipt.clone(), e.into()))?;
    io.sync(&plan.partition)
        .map_err(|e| failure(Stage::SyncPartition, true, receipt.clone(), e.into()))?;
    let mut readback = vec![0; plan.original.len()];
    io.read_back(&plan.partition, &mut readback)
        .map_err(|e| failure(Stage::ReadBack, true, receipt.clone(), e.into()))?;
    let verify = || -> Result<[u8; 32]> {
        ensure!(
            identity(&plan.partition, plan.regular_test_file)? == plan.identity,
            "partition identity/size changed during write"
        );
        ensure!(
            readback[FLAGS_OFFSET..FLAGS_OFFSET + 4] == new_flags.to_be_bytes(),
            "AVB flags did not read back as requested"
        );
        ensure!(
            readback[..FLAGS_OFFSET] == plan.original[..FLAGS_OFFSET]
                && readback[FLAGS_OFFSET + 4..] == plan.original[FLAGS_OFFSET + 4..],
            "bytes outside AVB flags changed"
        );
        ensure!(
            validate_image(&readback)?.flags == new_flags,
            "updated AVB header failed validation"
        );
        check_boot(current)?;
        if let Some((backup_file, parent, _)) = &held {
            backup_still_named(backup_file, parent, plan.backup.as_ref().expect("held backup has a path"), &plan.original)?;
        }
        Ok(digest(&readback))
    };
    outcome.result_sha256 =
        verify().map_err(|e| failure(Stage::ReadBack, true, receipt.clone(), e))?;
    outcome.changed = true;
    outcome.backup = receipt;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn u32_at(image: &mut [u8], offset: usize, value: u32) {
        image[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    fn u64_at(image: &mut [u8], offset: usize, value: u64) {
        image[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }
    fn image(flags: u32) -> Vec<u8> {
        let mut image = vec![0; 4096];
        image[..4].copy_from_slice(b"AVB0");
        u32_at(&mut image, 4, 1);
        u32_at(&mut image, 8, 3);
        u64_at(&mut image, 20, 64);
        u32_at(&mut image, 120, flags);
        image[128..141].copy_from_slice(b"avbtool 1.3.0");
        // Non-header and partition-tail bytes must all survive unchanged.
        for (i, b) in image[256..].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        image
    }
    fn current() -> CurrentDevice {
        CurrentDevice::new(
            "venus",
            fs::read_to_string(BOOT_ID).unwrap().trim(),
            "_a",
            "unlocked",
            "orange",
            "0",
        )
        .unwrap()
    }
    struct Fixture {
        _dir: TempDir,
        path: PathBuf,
        backup: PathBuf,
        device: CurrentDevice,
        original: Vec<u8>,
    }
    impl Fixture {
        fn new(flags: u32) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("vbmeta_a");
            let backup = dir.path().join("original-vbmeta_a.img");
            let original = image(flags);
            fs::write(&path, &original).unwrap();
            Self {
                _dir: dir,
                path,
                backup,
                device: current(),
                original,
            }
        }
        fn plan(&self) -> ValidatedPlan {
            prepare_at(&self.device, &self.path, &self.backup, true).unwrap()
        }
        fn unchanged(&self) {
            assert_eq!(fs::read(&self.path).unwrap(), self.original);
        }
    }

    #[test]
    fn unlock_requires_positive_consistent_evidence() {
        let id = current().boot_id;
        for (state, verified, locked) in [
            ("", "", ""),
            ("", "", "0"),
            ("locked", "orange", "0"),
            ("unlocked", "green", "0"),
            ("unlocked", "yellow", "0"),
            ("unlocked", "red", "0"),
            ("unlocked", "orange", "1"),
            ("unknown", "", ""),
            (" unlocked", "", ""),
        ] {
            assert!(CurrentDevice::new("venus", &id, "_a", state, verified, locked).is_err());
        }
        assert!(CurrentDevice::new("venus", &id, "_b", "", "orange", "").is_ok());
        assert!(CurrentDevice::new("venus", &id, "_a", "unlocked", "", "").is_ok());
        for slot in ["", "a", "_c", "../vbmeta_a"] {
            assert!(CurrentDevice::new("venus", &id, slot, "unlocked", "", "").is_err());
        }
        assert!(CurrentDevice::new("other", &id, "_a", "unlocked", "", "").is_err());
        assert!(CurrentDevice::new("venus", "", "_a", "unlocked", "", "").is_err());
    }

    #[test]
    fn header_rejects_truncation_bad_magic_versions_reserved_and_release() {
        let valid = image(0x8000_0000);
        assert_eq!(validate_image(&valid).unwrap().flags, 0x8000_0000);
        for n in 0..256 {
            assert!(validate_image(&valid[..n]).is_err());
        }
        for (offset, value) in [(0, 0), (4, 0), (4, 2), (8, 4), (124, 32)] {
            let mut bad = valid.clone();
            u32_at(&mut bad, offset, value);
            assert!(validate_image(&bad).is_err());
        }
        for offset in [175, 176, 255] {
            let mut bad = valid.clone();
            bad[offset] = 1;
            assert!(validate_image(&bad).is_err());
        }
    }

    #[test]
    fn header_checks_all_region_bounds_and_integer_overflows() {
        let valid = image(0);
        for (offset, value) in [
            (12, 1),
            (20, 1),
            (12, u64::MAX - 63),
            (20, 8192),
            (32, 1),
            (48, 1),
            (64, 65),
            (80, 65),
            (96, 65),
            (104, 72),
        ] {
            let mut bad = valid.clone();
            u64_at(&mut bad, offset, value);
            assert!(validate_image(&bad).is_err(), "offset {offset}");
        }
        for (offset, size) in [(32, 40), (48, 56), (64, 72), (80, 88), (96, 104)] {
            let mut bad = valid.clone();
            u64_at(&mut bad, offset, u64::MAX);
            u64_at(&mut bad, size, 8);
            assert!(validate_image(&bad).is_err());
        }
    }

    #[test]
    fn signed_algorithm_sizes_overlap_and_descriptor_walk_are_checked() {
        let mut valid = image(0);
        u64_at(&mut valid, 12, 320);
        u64_at(&mut valid, 20, 576);
        u32_at(&mut valid, 28, 1);
        u64_at(&mut valid, 40, 32);
        u64_at(&mut valid, 48, 32);
        u64_at(&mut valid, 56, 256);
        u64_at(&mut valid, 72, 520);
        u64_at(&mut valid, 96, 520);
        u64_at(&mut valid, 104, 24);
        let d = 256 + 320 + 520;
        valid[d..d + 24].fill(0);
        u64_at(&mut valid, d + 8, 8);
        assert!(validate_image(&valid).is_ok());
        for (offset, value) in [
            (40, 31),
            (48, 0),
            (72, 512),
            (96, 512),
            (104, 23),
            (d + 8, 9),
            (d + 8, 16),
            (d + 8, u64::MAX - 7),
        ] {
            let mut bad = valid.clone();
            u64_at(&mut bad, offset, value);
            assert!(
                validate_image(&bad).is_err(),
                "offset {offset}, value {value}"
            );
        }
        let mut bad = valid.clone();
        u32_at(&mut bad, 28, 7);
        assert!(validate_image(&bad).is_err());
    }

    #[test]
    fn only_four_flag_bytes_change_and_complete_original_is_backed_up() {
        let f = Fixture::new(0x8000_0100);
        let result = execute(f.plan(), &f.device).unwrap();
        assert!(result.changed);
        assert_eq!(result.old_flags, 0x8000_0100);
        assert_eq!(result.new_flags, 0x8000_0103);
        assert_eq!(fs::read(&f.backup).unwrap(), f.original);
        assert_eq!(fs::metadata(&f.backup).unwrap().mode() & 0o777, 0o600);
        let mut expected = f.original.clone();
        u32_at(&mut expected, 120, 0x8000_0103);
        assert_eq!(fs::read(&f.path).unwrap(), expected);
        assert_eq!(result.original_sha256, digest(&f.original));
        assert_eq!(result.result_sha256, digest(&expected));
        assert_eq!(result.backup.unwrap().bytes, 4096);
    }

    #[test]
    fn already_disabled_is_explicit_no_write_and_no_backup() {
        let f = Fixture::new(0x8000_0003);
        let result = execute(f.plan(), &f.device).unwrap();
        assert!(!result.changed);
        assert!(result.backup.is_none());
        assert!(!f.backup.exists());
        f.unchanged();
    }

    #[test]
    fn existing_backup_and_symlinks_are_never_replaced() {
        let f = Fixture::new(0);
        fs::write(&f.backup, b"keep").unwrap();
        assert!(prepare_at(&f.device, &f.path, &f.backup, true).is_err());
        assert_eq!(fs::read(&f.backup).unwrap(), b"keep");
        f.unchanged();
        fs::remove_file(&f.backup).unwrap();
        symlink(&f.path, &f.backup).unwrap();
        assert!(prepare_at(&f.device, &f.path, &f.backup, true).is_err());
        f.unchanged();
        let alias = f._dir.path().join("alias");
        symlink(f._dir.path(), &alias).unwrap();
        assert!(prepare_at(&f.device, &f.path, &alias.join("new.img"), true).is_err());
        assert!(open_backup_parent(&f._dir.path().join("./new.img")).is_err());
    }

    #[test]
    fn backup_collision_after_prepare_prevents_partition_write() {
        let f = Fixture::new(0);
        let plan = f.plan();
        fs::write(&f.backup, b"keep").unwrap();
        let err = execute(plan, &f.device).unwrap_err();
        assert_eq!(err.stage, Stage::Backup);
        assert!(!err.partition_may_have_changed);
        f.unchanged();
        assert_eq!(fs::read(&f.backup).unwrap(), b"keep");
    }

    #[test]
    fn verified_backup_cannot_be_silently_modified_or_replaced() {
        let f = Fixture::new(0);
        let plan = f.plan();
        let (file, parent, _) = backup(&plan, &f.backup, &mut RealIo).unwrap();
        assert!(backup_still_named(&file, &parent, &f.backup, &f.original).is_ok());
        file.write_all_at(b"bad", 1000).unwrap();
        assert!(backup_still_named(&file, &parent, &f.backup, &f.original).is_err());
        file.write_all_at(&f.original, 0).unwrap();
        fs::rename(&f.backup, f._dir.path().join("moved.img")).unwrap();
        fs::write(&f.backup, &f.original).unwrap();
        assert!(backup_still_named(&file, &parent, &f.backup, &f.original).is_err());
        f.unchanged();
    }

    #[test]
    fn prepare_refuses_the_wrong_partition_stale_boot_and_nonblock_production_source() {
        let f = Fixture::new(0);
        let wrong = f._dir.path().join("vbmeta_system_a");
        fs::write(&wrong, &f.original).unwrap();
        assert!(prepare_at(&f.device, &wrong, &f.backup, true).is_err());
        assert!(prepare_at(&f.device, &f.path, &f.backup, false).is_err());
        let mut stale = f.device.clone();
        stale.boot_id = "00000000-0000-0000-0000-000000000000".into();
        assert!(prepare_at(&stale, &f.path, &f.backup, true).is_err());
    }

    #[test]
    fn competing_plan_is_locked_out_and_drop_releases_lock() {
        let f = Fixture::new(0);
        let plan = f.plan();
        assert!(prepare_at(&f.device, &f.path, &f.backup, true).is_err());
        drop(plan);
        assert!(prepare_at(&f.device, &f.path, &f.backup, true).is_ok());
    }

    #[test]
    fn stale_contents_replaced_path_changed_slot_or_length_never_write() {
        let f = Fixture::new(0);
        let plan = f.plan();
        let file = OpenOptions::new().write(true).open(&f.path).unwrap();
        file.write_all_at(b"x", 1000).unwrap();
        let changed = fs::read(&f.path).unwrap();
        let err = execute(plan, &f.device).unwrap_err();
        assert!(!err.partition_may_have_changed);
        assert_eq!(fs::read(&f.path).unwrap(), changed);
        assert!(!f.backup.exists());
        let f = Fixture::new(0);
        let plan = f.plan();
        let old = f._dir.path().join("old");
        fs::rename(&f.path, &old).unwrap();
        fs::write(&f.path, &f.original).unwrap();
        assert!(execute(plan, &f.device).is_err());
        assert_eq!(fs::read(&old).unwrap(), f.original);
        f.unchanged();
        let f = Fixture::new(0);
        let plan = f.plan();
        let mut changed = f.device.clone();
        changed.slot_suffix = "_b".into();
        assert!(execute(plan, &changed).is_err());
        f.unchanged();
        let f = Fixture::new(0);
        let plan = f.plan();
        OpenOptions::new()
            .write(true)
            .open(&f.path)
            .unwrap()
            .set_len(2048)
            .unwrap();
        assert!(execute(plan, &f.device).is_err());
        assert!(!f.backup.exists());
    }

    #[derive(Clone, Copy)]
    enum Fault {
        BackupSync,
        ShortWrite,
        Sync,
        Read,
        OutsideFlags,
    }
    struct FaultIo {
        fault: Fault,
        writes: usize,
        backup_synced: bool,
        backup_path: PathBuf,
        original: Vec<u8>,
    }
    impl PartitionIo for FaultIo {
        fn sync_backup(&mut self, file: &File, parent: &File) -> io::Result<()> {
            if matches!(self.fault, Fault::BackupSync) {
                return Err(io::Error::other("injected backup fsync failure"));
            }
            file.sync_all()?;
            parent.sync_all()?;
            self.backup_synced = true;
            Ok(())
        }
        fn write_image(&mut self, file: &File, image: &[u8]) -> io::Result<()> {
            assert!(self.backup_synced);
            assert_eq!(fs::read(&self.backup_path).unwrap(), self.original);
            self.writes += 1;
            if matches!(self.fault, Fault::ShortWrite) {
                file.write_all_at(&image[..3], 0)?;
                return Err(io::Error::other("injected partial write"));
            }
            file.write_all_at(image, 0)
        }
        fn sync(&mut self, file: &File) -> io::Result<()> {
            if matches!(self.fault, Fault::Sync) {
                return Err(io::Error::other("injected partition fsync failure"));
            }
            file.sync_all()
        }
        fn read_back(&mut self, file: &File, bytes: &mut [u8]) -> io::Result<()> {
            if matches!(self.fault, Fault::Read) {
                return Err(io::Error::other("injected read-back failure"));
            }
            if matches!(self.fault, Fault::OutsideFlags) {
                file.write_all_at(b"bad", 1000)?;
            }
            file.read_exact_at(bytes, 0)
        }
    }

    #[test]
    fn every_commit_failure_is_reported_without_false_success_or_automatic_rollback() {
        for (fault, stage) in [
            (Fault::BackupSync, Stage::Backup),
            (Fault::ShortWrite, Stage::WriteFlags),
            (Fault::Sync, Stage::SyncPartition),
            (Fault::Read, Stage::ReadBack),
            (Fault::OutsideFlags, Stage::ReadBack),
        ] {
            let f = Fixture::new(0);
            let mut io = FaultIo {
                fault,
                writes: 0,
                backup_synced: false,
                backup_path: f.backup.clone(),
                original: f.original.clone(),
            };
            let error = execute_with(f.plan(), &f.device, &mut io).unwrap_err();
            assert_eq!(error.stage, stage);
            assert_eq!(fs::read(&f.backup).unwrap(), f.original);
            if matches!(fault, Fault::BackupSync) {
                assert_eq!(io.writes, 0);
                assert!(!error.partition_may_have_changed);
                assert!(error.backup.is_none());
                f.unchanged();
            } else {
                assert_eq!(io.writes, 1);
                assert!(error.partition_may_have_changed);
                assert_eq!(error.backup.unwrap().sha256, digest(&f.original));
                let flags = be32(&fs::read(&f.path).unwrap(), 120);
                if matches!(fault, Fault::ShortWrite) {
                    assert_ne!(flags & DISABLE_BITS, DISABLE_BITS, "a truncated image does not claim both disable bits");
                } else {
                    assert_eq!(flags, 3);
                }
            }
        }
    }
}
