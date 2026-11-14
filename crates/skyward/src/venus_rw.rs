//! Venus recovery's writable userdata lifecycle. Diagnostic RO sessions stay
//! separate: never reload their live encryption table or mount userdata twice.
//! Keys stay in memory; the small session receipt contains identities only.
use anyhow::{ensure, Context, Result};
use skyward_block::fstab::Entry;

#[allow(dead_code)]
pub const MARKER: &str = "venus-recovery-rw-v1\n";
pub const MOUNT: &str = "/data";
pub const MAPPING: &str = "userdata";
#[cfg(target_os = "android")]
const RECORD: &str = "/tmp/skyward-rw-session.txt";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseStep {
    Sync,
    Storage,
    Data,
    Mapping,
    Receipt,
}

// Only enter after the session's ownership and UI/MTP preconditions pass.
// Hold the radio exclusion guard through the last cleanup so no new modem
// helper can inherit Data during teardown. Stop at the first failure;
// never discard the ownership receipt before the mapping is removed.
fn close_steps<G>(
    storage: bool,
    quiesce: impl FnOnce() -> Result<G>,
    mut run: impl FnMut(CloseStep) -> Result<()>,
) -> Result<()> {
    let _radio_storage = quiesce()?;
    run(CloseStep::Sync)?;
    if storage {
        run(CloseStep::Storage)?;
    }
    run(CloseStep::Data)?;
    run(CloseStep::Mapping)?;
    run(CloseStep::Receipt)
}

fn security_needs_start(state: Option<&str>) -> Result<bool> {
    match state {
        // Disabled services have no state property until their first start.
        None | Some("stopped") => Ok(true),
        Some("running") => Ok(false),
        _ => anyhow::bail!("venus_security is transitioning; wait for its current operation"),
    }
}

fn options(entry: &Entry) -> Result<Vec<String>> {
    ensure!(
        entry.mount_point == MOUNT && entry.filesystem == "f2fs",
        "expected the installed venus F2FS userdata entry"
    );
    ensure!(
        entry.extra("keydirectory") == Some("/metadata/vold/metadata_encryption"),
        "unsupported metadata key directory"
    );
    ensure!(
        entry.extra("metadata_encryption") == Some("aes-256-xts:wrappedkey_v0"),
        "unsupported metadata encryption mode"
    );
    ensure!(
        entry
            .extra("fileencryption")
            .is_some_and(|x| x.contains(":v2+") && x.ends_with("+wrappedkey_v0")),
        "unsupported file encryption policy"
    );
    // Not the diagnostic norecovery mount. Keep write barriers and allow the
    // normal filesystem recovery/checkpoint behavior; never run mkfs/fsck here.
    let mut out: Vec<String> = [
        "rw",
        "nosuid",
        "nodev",
        "noatime",
        "inlinecrypt",
        "fsync_mode=posix",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for x in &entry.options {
        if x.starts_with("reserve_root=") || x.starts_with("resgid=") {
            let (_, n) = x.split_once('=').context("invalid reserve option")?;
            ensure!(
                !n.is_empty() && n.bytes().all(|x| x.is_ascii_digit()),
                "invalid reserve option"
            );
            out.push(x.clone());
        }
    }
    Ok(out)
}

#[derive(Debug, PartialEq, Eq)]
struct Session {
    boot: String,
    source: (u32, u32),
    mapped: (u32, u32),
    mount_id: i32,
}
impl Session {
    fn encode(&self) -> String {
        format!(
            "venus-rw-session-v1\nboot={}\nsource={}:{}\nmapped={}:{}\nmount_id={}\n",
            self.boot, self.source.0, self.source.1, self.mapped.0, self.mapped.1, self.mount_id
        )
    }
    fn decode(raw: &str) -> Result<Self> {
        ensure!(raw.len() < 256, "oversized writable session record");
        let l: Vec<_> = raw.lines().collect();
        ensure!(
            l.len() == 5 && l[0] == "venus-rw-session-v1",
            "invalid writable session version"
        );
        let boot = l[1]
            .strip_prefix("boot=")
            .context("missing boot identity")?;
        ensure!(
            boot.len() == 36
                && boot
                    .bytes()
                    .enumerate()
                    .all(|(i, x)| if [8, 13, 18, 23].contains(&i) {
                        x == b'-'
                    } else {
                        x.is_ascii_hexdigit()
                    }),
            "invalid boot identity"
        );
        let dev = |line: &str, prefix: &str| -> Result<(u32, u32)> {
            let (a, b) = line
                .strip_prefix(prefix)
                .context("missing device identity")?
                .split_once(':')
                .context("bad device identity")?;
            ensure!(
                !a.is_empty()
                    && !b.is_empty()
                    && a.bytes().chain(b.bytes()).all(|x| x.is_ascii_digit()),
                "bad device number"
            );
            let pair = (a.parse()?, b.parse()?);
            ensure!(pair.0 != 0, "zero device major");
            Ok(pair)
        };
        let id = l[4]
            .strip_prefix("mount_id=")
            .context("missing mount ID")?
            .parse()?;
        ensure!(id > 0, "bad mount ID");
        Ok(Self {
            boot: boot.to_owned(),
            source: dev(l[2], "source=")?,
            mapped: dev(l[3], "mapped=")?,
            mount_id: id,
        })
    }
}

#[cfg(target_os = "android")]
mod runtime {
    use super::*;
    use skyward_binder::driver::Binder;
    use skyward_block::{dm, mount};
    use skyward_crypto::{
        ce_store::Profile,
        data::{Closed, Opened, Opening},
        fscrypt,
        keymint::KeyMint,
        keystore::{StoredKey, Unwrap},
    };
    use std::{
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::{
            fd::AsRawFd,
            unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        },
        path::Path,
    };
    use zeroize::{Zeroize, Zeroizing};
    const SOURCE: &str = "/dev/block/by-name/userdata";

    pub fn enabled() -> bool {
        fs::read_to_string("/skyward/venus-recovery.txt").is_ok_and(|s| s.starts_with(MARKER))
    }
    fn lock() -> Result<File> {
        ensure!(
            enabled() && unsafe { libc::geteuid() } == 0,
            "requires the marked venus Recovery and root"
        );
        let mut scratch: libc::statfs = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { libc::statfs(c"/tmp".as_ptr(), &mut scratch) } == 0
                && scratch.f_type as u64 == 0x01021994,
            "credential scratch must remain tmpfs"
        );
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/tmp/skyward-km41-data.lock")?;
        ensure!(
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "another userdata operation is running"
        );
        ensure!(
            skyward_vab::snapshot_state()? == skyward_vab::SnapshotState::None,
            "OTA must be known idle before writable Data"
        );
        Ok(f)
    }
    fn snapshot() -> Result<Session> {
        let mapped = dm::status(MAPPING)?.context("writable userdata mapping missing")?;
        ensure!(!dm::readonly(MAPPING)?, "userdata mapping is read-only");
        let visible =
            mount::visible(Path::new(MOUNT))?.context("writable userdata mount missing")?;
        ensure!(
            visible.device == mapped && visible.filesystem == "f2fs",
            "userdata mount identity differs"
        );
        let ids: Vec<_> = mount::mount_ids()?
            .into_iter()
            .filter(|(_, p)| p == Path::new(MOUNT))
            .collect();
        ensure!(ids.len() == 1, "stacked userdata mounts are not supported");
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { libc::statvfs(c"/data".as_ptr(), &mut st) } == 0,
            "cannot query userdata flags"
        );
        ensure!(
            st.f_flag & libc::ST_RDONLY == 0,
            "userdata filesystem is read-only"
        );
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .to_owned();
        Ok(Session {
            boot,
            source: skyward_block::devnum(SOURCE)?,
            mapped,
            mount_id: ids[0].0,
        })
    }
    pub fn verify() -> Result<()> {
        ensure!(enabled(), "not the writable venus Recovery");
        let m = fs::symlink_metadata(RECORD).context("no owned writable userdata session")?;
        ensure!(
            m.is_file() && m.uid() == 0 && m.mode() & 0o077 == 0 && m.len() < 256,
            "unsafe writable session receipt"
        );
        let saved = Session::decode(&fs::read_to_string(RECORD)?)?;
        ensure!(
            saved == snapshot()?,
            "userdata boot/device/mount identity changed"
        );
        Ok(())
    }
    fn save() -> Result<()> {
        let now = snapshot()?;
        let temp = format!("{RECORD}.new");
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)?;
        f.write_all(now.encode().as_bytes())?;
        f.sync_all()?;
        fs::rename(temp, RECORD)?;
        verify()
    }
    fn vendor_entry() -> Result<Entry> {
        let suffix = skyward_sys::prop("ro.boot.fstab_suffix").unwrap_or_else(|| "default".into());
        ensure!(
            matches!(suffix.as_str(), "default" | "emmc"),
            "unexpected vendor fstab suffix"
        );
        let original = std::path::PathBuf::from(format!("/mnt/skyward-ro/vendor/etc/fstab.{suffix}"));
        let path = cached_file(&original)?.unwrap_or(original);
        let text = fs::read_to_string(path)?;
        let mut found: Vec<_> = skyward_block::fstab::entries(&text)
            .into_iter()
            .filter(|e| e.mount_point == MOUNT)
            .collect();
        ensure!(found.len() == 1, "ambiguous installed userdata entry");
        let entry = found.remove(0);
        options(&entry)?;
        ensure!(
            fs::canonicalize(skyward_block::fstab::device(&entry)?)? == fs::canonicalize(SOURCE)?,
            "fstab names another device"
        );
        Ok(entry)
    }
    fn no_other_data_mount() -> Result<()> {
        ensure!(
            dm::status("skyward-km41-data-ro")?.is_none()
                && !mount::is_mount_point(Path::new("/mnt/skyward-data-ro"))?,
            "old read-only diagnostic session is still mounted; boot the RW image afresh, do not hot-reload or unmount that session"
        );
        ensure!(
            !mount::is_mount_point(Path::new(MOUNT))? && dm::status(MAPPING)?.is_none(),
            "unowned userdata mapping/mount already exists"
        );
        ensure!(
            mount::points_of(skyward_block::devnum(SOURCE)?)?.is_empty(),
            "raw userdata is mounted elsewhere"
        );
        // Refuse an unknown DM holder of the encrypted source, even under a
        // different name, before making a second view of the same filesystem.
        let real = fs::canonicalize(SOURCE)?;
        let name = real.file_name().context("userdata node has no name")?;
        let holders = Path::new("/sys/class/block").join(name).join("holders");
        ensure!(
            fs::read_dir(holders)?.next().is_none(),
            "userdata already has another device-mapper holder"
        );
        Ok(())
    }
    pub fn prepare_services() -> Result<()> {
        ensure!(enabled(), "not the writable venus Recovery");
        // A service stopped by init loses all its descendants. Security HALs
        // must belong to their own init service, not the restartable UI group.
        let service = || skyward_sys::prop("init.svc.venus_security");
        if security_needs_start(service().as_deref())? {
            ensure!(
                Path::new("/skyward/security-start.sh").is_file(),
                "missing venus security service wrapper; rebuild the integrated image"
            );
            ensure!(
                !mount::is_mount_point(Path::new(MOUNT))? && !Path::new(RECORD).exists(),
                "security service stopped with live Data; refusing automatic HAL restart"
            );
            skyward_sys::set_prop("ctl.start", "venus_security")?;
        }
        let started = std::time::Instant::now();
        loop {
            let state = service();
            if state.as_deref() == Some("running")
                && Path::new("/tmp/skyward-recovery/runtime-ready").is_file()
            {
                break;
            }
            ensure!(
                started.elapsed() < std::time::Duration::from_secs(90),
                "timed out waiting for venus_security; see /tmp/skyward-recovery/security.log"
            );
            // ctl.start is asynchronous. Allow its first transition to running.
            ensure!(
                state.as_deref() != Some("stopped")
                    || started.elapsed() < std::time::Duration::from_secs(2),
                "venus_security exited during setup; see /tmp/skyward-recovery/runtime.log"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // The fast path only verifies the existing owned processes and their
        // readiness; startup is performed solely in the independent service.
        let status = std::process::Command::new("/system/bin/sh")
            .arg("/skyward/rw-runtime.sh")
            .status()
            .context("prepare bundled HAL runtime")?;
        ensure!(
            status.success(),
            "bundled HAL startup failed; see /tmp/skyward-recovery/runtime.log"
        );
        Ok(())
    }
    pub fn open() -> Result<Opening> {
        // All HAL/assets setup happens before /data is mounted, and the
        // runtime's fast path is a no-op once its services remain ready.
        prepare_services()?;
        let _guard = lock()?;
        let hw = Binder::hardware()?;
        let km = KeyMint::open_hidl(&hw, false)?;
        let mounted = if Path::new(RECORD).exists() {
            verify()?;
            None
        } else {
            no_other_data_mount()?;
            let entry = vendor_entry()?;
            let opts = options(&entry)?;
            crate::venus_keymaster::phase("DATA_RW_METADATA_UNWRAP_BEGIN");
            let stored = StoredKey::load(Path::new("/metadata/vold/metadata_encryption/key"))?;
            let raw = Zeroizing::new(stored.unwrap(Unwrap::Hardware(&km))?);
            let ephemeral = Zeroizing::new(km.ephemeral_storage_key(&raw)?);
            ensure!(
                ephemeral.len() == 68,
                "unexpected wrapped metadata key length"
            );
            let size = skyward_block::size_bytes(SOURCE)?;
            ensure!(
                size >= 4096 && size.is_multiple_of(4096),
                "unaligned userdata size"
            );
            let mut target = dm::Target::DefaultKey {
                sectors: size / 512,
                device: SOURCE.into(),
                cipher: "aes-xts-plain64".into(),
                key: ephemeral.iter().map(|b| format!("{b:02x}")).collect(),
                wrapped: true,
            };
            let result = dm::create(MAPPING, std::slice::from_ref(&target), false);
            if let dm::Target::DefaultKey { key, .. } = &mut target {
                key.zeroize();
            }
            let mapped = result?;
            // Only remove a mapping that never mounted. Never run the old
            // diagnostic unmount path implicitly on a later validation error.
            let result = (|| -> Result<()> {
                ensure!(
                    !dm::readonly(MAPPING)?,
                    "writable mapping creation did not take"
                );
                let mut magic = [0u8; 4];
                File::open(&mapped)?.read_exact_at(&mut magic, 1024)?;
                ensure!(
                    u32::from_le_bytes(magic) == 0xf2f52010,
                    "decrypted F2FS superblock mismatch"
                );
                let m = fs::symlink_metadata(MOUNT)?;
                ensure!(
                    m.is_dir() && !m.file_type().is_symlink(),
                    "unsafe /data mount point"
                );
                crate::venus_keymaster::phase("DATA_RW_MOUNT_BEGIN");
                mount::mount(&mapped, Path::new(MOUNT), "f2fs", &opts)
                    .context("mount real userdata read/write")
            })();
            if let Err(e) = result {
                let cleanup = dm::remove(MAPPING);
                return match cleanup {
                    Ok(()) => Err(e),
                    Err(c) => Err(e.context(format!("unmounted mapping removal failed: {c:#}"))),
                };
            }
            save().context(
                "mounted writable Data retained; session receipt failed, no automatic teardown",
            )?;
            crate::venus_keymaster::phase("DATA_RW_MOUNT_OK");
            println!("VENUS_DATA_RW_MOUNT_OK mapping_readonly=false filesystem_readonly=false");
            Some(Opened {
                mapped,
                filesystem: "f2fs".into(),
                checked: None,
            })
        };
        crate::venus_data_ro::verify_de(&km, Path::new(MOUNT))?;
        verify()?;
        let profile = Profile::load(Path::new(MOUNT), Path::new("/tmp"))?;
        let closed = if fscrypt::unlocked(Path::new("/data/media/0"))? {
            show_storage()?;
            Vec::new()
        } else if let Some(l) = &profile.lock {
            ensure!(
                matches!(profile.kind(), "pin" | "password"),
                "this verified backend supports PIN/password, not a guessed pattern encoding"
            );
            vec![Closed {
                user: 0,
                lock: l.kind,
                made_up: false,
                why: None,
            }]
        } else {
            crate::venus_ce::unlock_at(
                Path::new(MOUNT),
                &profile,
                &km,
                &skyward_crypto::gatekeeper::Gatekeeper::open_hidl(&hw)?,
                &[],
                verify,
            )?;
            show_storage()?;
            Vec::new()
        };
        profile.unchanged()?;
        Ok(Opening { mounted, closed })
    }
    pub fn unlock(closed: &[Closed], user: u32, credential: &[u8]) -> Result<Vec<Closed>> {
        let _guard = lock()?;
        verify()?;
        ensure!(
            user == 0 && closed.iter().any(|c| c.user == 0),
            "this backend only unlocks the owner"
        );
        let hw = Binder::hardware()?;
        let km = KeyMint::open_hidl(&hw, false)?;
        let gk = skyward_crypto::gatekeeper::Gatekeeper::open_hidl(&hw)?;
        let profile = Profile::load(Path::new(MOUNT), Path::new("/tmp"))?;
        let result =
            crate::venus_ce::unlock_at(Path::new(MOUNT), &profile, &km, &gk, credential, verify);
        profile.unchanged()?;
        verify()?;
        result?;
        show_storage()?;
        println!("VENUS_RECOVERY_CE_UNLOCK_OK user=0 readonly=false shared_storage_unlocked=true");
        Ok(closed.iter().filter(|c| c.user != 0).cloned().collect())
    }
    fn show_storage() -> Result<()> {
        verify()?;
        ensure!(
            fscrypt::unlocked(Path::new("/data/media/0"))?,
            "CE must be unlocked before exposing internal storage"
        );
        let to = Path::new("/sdcard");
        if mount::is_mount_point(to)? {
            let a = fs::metadata("/data/media/0")?;
            let b = fs::metadata(to)?;
            ensure!(
                a.dev() == b.dev() && a.ino() == b.ino(),
                "/sdcard belongs to another tree"
            );
        } else {
            let m = fs::symlink_metadata(to)?;
            ensure!(
                m.is_dir() && !m.file_type().is_symlink(),
                "/sdcard is not a plain directory"
            );
            mount::bind(Path::new("/data/media/0"), to)?;
        }
        Ok(())
    }
    pub fn status() -> Result<()> {
        verify()?;
        let ce = fscrypt::unlocked(Path::new("/data/media/0"))?;
        println!(
            "VENUS_RECOVERY_DATA_STATUS user=0 mounted=true mapping_readonly=false filesystem_readonly=false ce_unlocked={ce}"
        );
        Ok(())
    }
    /// Explicit lifecycle test. The UI (including its workers) must have
    /// exited and USB must be back on adb before any filesystem is touched.
    /// Partial failures retain the receipt for diagnosis; no force/lazy unmount.
    pub fn close_session() -> Result<()> {
        let _guard = lock()?;
        ensure!(
            skyward_sys::prop("init.svc.venus_ui").as_deref() == Some("stopped"),
            "stop venus_ui and its workers before closing Data"
        );
        ensure!(
            skyward_sys::prop("sys.usb.state").as_deref() == Some("adb")
                && skyward_sys::prop("sys.usb.ffs.mtp.ready").as_deref() != Some("1"),
            "turn off MTP and restore adb before closing Data"
        );
        verify()?;
        let storage = mount::is_mount_point(Path::new("/sdcard"))?;
        if storage {
            let a = fs::metadata("/data/media/0")?;
            let b = fs::metadata("/sdcard")?;
            ensure!(
                a.dev() == b.dev() && a.ino() == b.ino(),
                "foreign /sdcard mount"
            );
        }
        let mapped = snapshot()?.mapped;
        for (_, point) in mount::mount_ids()? {
            ensure!(
                !(point.starts_with(MOUNT) && point != Path::new(MOUNT))
                    && !(point.starts_with("/sdcard") && point != Path::new("/sdcard")),
                "nested mount must be released first: {}",
                point.display()
            );
        }
        for point in mount::points_of(mapped)? {
            ensure!(
                point == Path::new(MOUNT) || point == Path::new("/sdcard"),
                "userdata is also mounted at {}",
                point.display()
            );
        }
        close_steps(storage, || {
            crate::venus_keymaster::phase("DATA_RW_CLOSE_RADIO_QUIESCE_BEGIN");
            let guard = crate::venus_modem::efs_guard()?;
            crate::venus_keymaster::phase("DATA_RW_CLOSE_RADIO_QUIESCE_OK");
            Ok(guard)
        }, |step| {
            let name = match step {
                CloseStep::Sync => "SYNC",
                CloseStep::Storage => "SDCARD_UMOUNT",
                CloseStep::Data => "UMOUNT",
                CloseStep::Mapping => "DM_REMOVE",
                CloseStep::Receipt => "RECEIPT_REMOVE",
            };
            crate::venus_keymaster::phase(&format!("DATA_RW_CLOSE_{name}_BEGIN"));
            match step {
                CloseStep::Sync => {
                    let f = File::open(MOUNT)?;
                    if unsafe { libc::syncfs(f.as_raw_fd()) } != 0 {
                        return Err(std::io::Error::last_os_error()).context("sync writable Data");
                    }
                    // Drop the file before umount: it would itself hold Data busy.
                }
                CloseStep::Storage => mount::umount(Path::new("/sdcard"))?,
                CloseStep::Data => mount::umount(Path::new(MOUNT))?,
                CloseStep::Mapping => {
                    ensure!(
                        mount::points_of(mapped)?.is_empty(),
                        "userdata is still mounted"
                    );
                    dm::remove(MAPPING)?;
                    ensure!(
                        dm::status(MAPPING)?.is_none(),
                        "userdata mapping survived removal"
                    );
                }
                CloseStep::Receipt => fs::remove_file(RECORD)?,
            }
            crate::venus_keymaster::phase(&format!("DATA_RW_CLOSE_{name}_OK"));
            Ok(())
        })?;
        println!("VENUS_DATA_RW_CLOSE_OK synced=true mounts_removed=true mapping_removed=true receipt_removed=true");
        Ok(())
    }
    pub fn write_test() -> Result<()> {
        let _guard = lock()?;
        verify()?;
        ensure!(
            fscrypt::unlocked(Path::new("/data/media/0"))?,
            "unlock CE before the write test"
        );
        let root = Path::new("/data/media/0");
        let nonce = fs::read_to_string("/proc/sys/kernel/random/uuid")?;
        ensure!(
            nonce
                .trim()
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b'-'),
            "bad random name"
        );
        let folder = root.join(format!(".skyward-rw-test-{}", nonce.trim()));
        // The test touches only a newly created unique directory. No existing
        // personal filenames/content are read, replaced, or removed.
        fs::create_dir(&folder)?;
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700))?;
        let path = folder.join("test.bin");
        let moved = folder.join("renamed.bin");
        let data: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        f.write_all(&data)?;
        f.sync_all()?;
        drop(f);
        ensure!(fs::read(&path)? == data, "write-test data mismatch");
        fs::rename(&path, &moved)?;
        File::open(&folder)?.sync_all()?;
        let mut read = Vec::new();
        File::open(&moved)?.read_to_end(&mut read)?;
        ensure!(read == data, "rename/readback mismatch");
        fs::remove_file(&moved)?;
        fs::remove_dir(&folder)?;
        File::open(root)?.sync_all()?;
        verify()?;
        println!(
            "VENUS_DATA_RW_TEST_OK bytes=8192 create_write_fsync_read_rename_delete=true existing_files_touched=false"
        );
        Ok(())
    }
    pub fn cache_root() -> Result<Option<std::path::PathBuf>> {
        let at = Path::new("/tmp/skyward-km41/cache");
        let meta = match fs::symlink_metadata(at) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("inspect RAM runtime cache"),
        };
        ensure!(meta.is_dir() && meta.uid() == 0 && meta.mode() & 0o077 == 0, "RAM runtime cache must be a private root-owned directory");
        let mut filesystem: libc::statfs = unsafe { std::mem::zeroed() };
        ensure!(unsafe { libc::statfs(c"/tmp/skyward-km41/cache".as_ptr(), &mut filesystem) } == 0
            && filesystem.f_type as u64 == 0x01021994, "runtime cache must remain tmpfs");
        let mut receipt = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(at.join("ready"))?;
        let meta = receipt.metadata()?;
        ensure!(meta.is_file() && meta.uid() == 0 && meta.mode() & 0o022 == 0 && meta.len() < 128, "invalid runtime cache receipt");
        let mut raw = String::new();
        receipt.read_to_string(&mut raw)?;
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        ensure!(raw == format!("venus-runtime-cache-v1\nboot={}\n", boot.trim()), "stale RAM runtime cache");
        Ok(Some(at.to_owned()))
    }
    pub fn cached_file(path: &Path) -> Result<Option<std::path::PathBuf>> {
        if !enabled() { return Ok(None); }
        let Ok(relative) = path.strip_prefix("/mnt/skyward-ro") else { return Ok(None) };
        ensure!(relative.components().all(|c| matches!(c, std::path::Component::Normal(_))), "invalid cached runtime path");
        Ok(cache_root()?.map(|root| root.join(relative)))
    }
    pub fn info(cfg: &skyward_config::DeviceConfig) -> Result<skyward_info::DeviceInfo> {
        let mut parts = Vec::new();
        let cache = cache_root()?;
        for base in ["system", "system_ext", "system_dlkm", "vendor", "vendor_dlkm", "odm", "odm_dlkm", "product", "mi_ext"] {
            let at = Path::new("/mnt/skyward-ro").join(base);
            // Once cached, background readings must not reopen a block
            // source while the install worker is trying to release it.
            if let Some(at) = cache.as_ref().map(|root| root.join(base)).filter(|at| at.is_dir()) {
                parts.push((base.to_string(), at));
            } else if cache.is_none() && at.exists() && mount::is_mount_point(&at)? {
                parts.push((base.to_string(), at));
            }
        }
        skyward_info::collect_with_installed_props(cfg, skyward_info::installed_props_from_mounts(&parts)?)
    }

    pub fn run_ui(config: &Path, dump: Option<&Path>) -> Result<()> {
        ensure!(
            enabled(),
            "use the explicitly built writable venus Recovery image"
        );
        let cfg = skyward_config::DeviceConfig::load(config)?;
        ensure!(cfg.device.codename == "venus", "unexpected device config");
        fs::create_dir_all(skyward_sys::RUN)?;
        fs::set_permissions(skyward_sys::RUN, fs::Permissions::from_mode(0o755))?;
        // Do not consume a stale BCB request as an automatic erase/install.
        // All partition-changing operations require an explicit UI action.
        if dump.is_none() {
            let _ = skyward_sys::clock::set_from_rtc();
        }
        crate::ui::run(
            &cfg,
            &cfg.services,
            Path::new("/skyward/MiSansVF.ttf"),
            Path::new("/skyward/KirkMono-Regular.ttf"),
            skyward_sys::bcb::Request::default(),
            skyward_ui::app::Screen::Decrypt,
            dump,
        )
    }
}
#[cfg(target_os = "android")]
pub use runtime::*;

/// A known resetting unmount path must not be reached accidentally by the
/// full UI. This does not turn userdata read-only: file writes and backups
/// run normally. Remove individual gates only after their real lifecycle test.
#[cfg(target_os = "android")]
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn never_started_disabled_security_service_can_start() {
        assert!(security_needs_start(None).unwrap());
        assert!(security_needs_start(Some("stopped")).unwrap());
        assert!(!security_needs_start(Some("running")).unwrap());
        assert!(security_needs_start(Some("restarting")).is_err());
        assert!(security_needs_start(Some("stopping")).is_err());
    }
    #[test]
    fn close_aborts_at_each_failure_without_later_cleanup() {
        let expected = [
            CloseStep::Sync,
            CloseStep::Storage,
            CloseStep::Data,
            CloseStep::Mapping,
            CloseStep::Receipt,
        ];
        for fail in 0..expected.len() {
            let mut seen = Vec::new();
            let result = close_steps(true, || Ok(()), |step| {
                seen.push(step);
                ensure!(
                    step != expected[fail],
                    "injected sync/unmount/remove failure"
                );
                Ok(())
            });
            assert!(result.is_err());
            assert_eq!(seen, expected[..=fail]);
        }
    }
    #[test]
    fn close_locked_storage_still_syncs_and_removes_receipt_last() {
        let mut seen = Vec::new();
        close_steps(false, || Ok(()), |step| {
            seen.push(step);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            seen,
            [
                CloseStep::Sync,
                CloseStep::Data,
                CloseStep::Mapping,
                CloseStep::Receipt
            ]
        );
    }
    #[test]
    fn close_never_syncs_or_unmounts_when_radio_quiesce_fails() {
        let mut seen = Vec::new();
        let result = close_steps(true, || -> Result<()> {
            anyhow::bail!("radio still owns its NV helpers")
        }, |step| {
            seen.push(step);
            Ok(())
        });
        assert!(result.is_err());
        assert!(seen.is_empty());
    }
    #[test]
    fn close_keeps_radio_excluded_until_success_or_aborted_cleanup() {
        use std::{cell::Cell, rc::Rc};
        struct Guard(Rc<Cell<bool>>);
        impl Drop for Guard {
            fn drop(&mut self) { self.0.set(false); }
        }
        for fail in [None, Some(CloseStep::Data)] {
            let held = Rc::new(Cell::new(false));
            let mut seen = Vec::new();
            let result = close_steps(true, || {
                held.set(true);
                Ok(Guard(held.clone()))
            }, |step| {
                assert!(held.get(), "radio exclusion released during {step:?}");
                seen.push(step);
                ensure!(Some(step) != fail, "injected teardown failure");
                Ok(())
            });
            assert_eq!(result.is_err(), fail.is_some());
            assert_eq!(seen.last().copied(), fail.or(Some(CloseStep::Receipt)));
            assert!(!held.get(), "radio exclusion leaked after close returned");
        }
    }
    fn entry() -> Entry {
        skyward_block::fstab::entries("/dev/block/bootdevice/by-name/userdata /data f2fs noatime,nosuid,nodev,discard,inlinecrypt,reserve_root=32768,resgid=1065,fsync_mode=nobarrier wait,fileencryption=aes-256-xts:aes-256-cts:v2+inlinecrypt_optimized+wrappedkey_v0,keydirectory=/metadata/vold/metadata_encryption,metadata_encryption=aes-256-xts:wrappedkey_v0").remove(0)
    }
    #[test]
    fn real_writable_mount_is_not_a_renamed_readonly_probe() {
        let opts = options(&entry()).unwrap();
        let (flags, data) = skyward_block::mount::split_options(&opts);
        assert_eq!(flags & 1, 0);
        assert!(opts.contains(&"rw".into()));
        assert!(data.contains("inlinecrypt"));
        assert!(data.contains("fsync_mode=posix"));
        for x in [
            "norecovery",
            "disable_roll_forward",
            "nobarrier",
            "checkpoint=disable",
            "test_dummy_encryption",
        ] {
            assert!(!data.contains(x));
        }
    }
    #[test]
    fn foreign_cipher_filesystem_and_metadata_directory_are_refused() {
        let mut e = entry();
        e.filesystem = "ext4".into();
        assert!(options(&e).is_err());
        let mut e = entry();
        e.extras.retain(|s| !s.starts_with("metadata_encryption="));
        assert!(options(&e).is_err());
        let mut e = entry();
        e.mount_point = "/metadata".into();
        assert!(options(&e).is_err());
    }
    #[test]
    fn writable_receipt_roundtrip_and_modes_are_not_mixed() {
        let s = Session {
            boot: "4a23f017-b8b0-4d24-a124-506701f455d8".into(),
            source: (259, 18),
            mapped: (253, 5),
            mount_id: 184,
        };
        assert_eq!(Session::decode(&s.encode()).unwrap(), s);
        assert!(Session::decode(
            &s.encode()
                .replace("venus-rw-session-v1", "venus-readonly-data-session-v1")
        )
        .is_err());
        assert!(Session::decode(&(s.encode() + "extra\n")).is_err());
        assert!(Session::decode(&s.encode().replace("mount_id=184", "mount_id=0")).is_err());
        assert!(Session::decode(&s.encode().replace("source=259:18", "source=0:18")).is_err());
    }
    #[test]
    fn unsafe_reserve_option_refused() {
        let mut e = entry();
        e.options.push("resgid=1,ro".into());
        assert!(options(&e).is_err());
    }
}
