//! Venus recovery's writable userdata lifecycle. Diagnostic RO sessions stay
//! separate: never reload their live encryption table or mount userdata twice.
//! Keys stay in memory; the small session receipt contains identities only.
use anyhow::{Context, Result, ensure};
use skyward_block::fstab::Entry;

#[allow(dead_code)]
pub const MARKER: &str = "venus-recovery-rw-v1\n";
pub const MOUNT: &str = "/data";
pub const MAPPING: &str = "userdata";
#[cfg(target_os = "android")]
const RECORD: &str = "/skyward/run/state/data-rw.txt";

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

/// MetadataCrypt makes an initial fs_mgr mount attempt. Its caller then
/// performs up to ten Mount checks and one final check, 500 us apart.
fn mount_metadata(mut attempt: impl FnMut() -> Result<()>, mut pause: impl FnMut()) -> Result<()> {
    if attempt().is_ok() {
        return Ok(());
    }
    for retry in 0..10 {
        if attempt().is_ok() {
            return Ok(());
        }
        if retry < 9 {
            pause();
        }
    }
    attempt()
}

fn options(entry: &Entry) -> Result<Vec<String>> {
    ensure!(
        entry.mount_point == MOUNT && matches!(entry.filesystem.as_str(), "f2fs" | "ext4"),
        "expected an installed userdata filesystem"
    );
    skyward_crypto::encryption::Encryption::of(entry)?;
    skyward_crypto::encryption::key_dir(entry)?;
    ensure!(
        entry.extra("fileencryption").is_some(),
        "missing file encryption policy"
    );
    // Preserve the effective additional-fstab options, including its fsync
    // mode. Do not replace them with the old diagnostic/posix option set.
    ensure!(
        entry
            .options
            .iter()
            .all(|s| !s.contains(',') && !s.contains('\0')),
        "invalid individual mount option"
    );
    let mut options: Vec<_> = entry
        .options
        .iter()
        .filter(|s| !matches!(s.as_str(), "defaults" | "ro" | "rw"))
        .cloned()
        .collect();
    options.push("rw".into());
    Ok(options)
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
        data::{Closed, Opened, Opening},
        fscrypt,
        keystore::{StoredKey, Unwrap},
        keystore2_client::Keystore,
        recovery::{Initialization, Session as CryptoSession},
    };
    use std::{
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
        path::Path,
    };
    use zeroize::{Zeroize, Zeroizing};
    static CRYPTO: std::sync::Mutex<Option<CryptoSession>> = std::sync::Mutex::new(None);
    static INITIALIZING: std::sync::Mutex<Option<Initialization>> = std::sync::Mutex::new(None);
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
            unsafe { libc::statfs(c"/skyward/run/crypto/tmp".as_ptr(), &mut scratch) } == 0
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
            .open("/skyward/run/locks/data.lock")?;
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
            visible.device == mapped && matches!(visible.filesystem.as_str(), "f2fs" | "ext4"),
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
        // Match TWFunc::GetFstabPath: suffix selection, then ODM before
        // vendor. Installed additional fstab overrides the recovery entry.
        let mut selected = None;
        for property in [
            "ro.boot.fstab_suffix",
            "ro.boot.hardware",
            "ro.boot.hardware.platform",
        ] {
            let Some(suffix) = skyward_sys::prop(property) else {
                continue;
            };
            ensure!(
                !suffix.is_empty()
                    && suffix
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)),
                "invalid fstab suffix"
            );
            for partition in ["odm", "vendor", "system/system"] {
                let original = std::path::PathBuf::from(format!(
                    "/mnt/skyward-ro/{partition}/etc/fstab.{suffix}"
                ));
                let path = cached_file(&original)?.unwrap_or(original);
                if path.is_file() {
                    selected = Some(path);
                    break;
                }
            }
            if selected.is_some() {
                break;
            }
        }
        let path = selected.unwrap_or_else(|| {
            std::path::PathBuf::from("/skyward/crypto-runtime/system/etc/recovery.fstab")
        });
        let text = fs::read_to_string(path)?;
        fs::write("/skyward/run/state/additional.fstab", &text)?;
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
        if let Ok(phase) = fs::read_to_string("/skyward/run/state/recovery/setup-failed") {
            anyhow::bail!(
                "security setup previously failed at {}; boot a fresh recovery; see /skyward/run/state/recovery/runtime.log and /skyward/run/crypto/verify.log",
                phase.trim()
            );
        }
        if security_needs_start(service().as_deref())? {
            ensure!(
                Path::new("/skyward/runtime/rw-runtime").is_file(),
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
                && Path::new("/skyward/run/state/recovery/crypto-ready").is_file()
            {
                break;
            }
            ensure!(
                started.elapsed() < std::time::Duration::from_secs(90),
                "timed out waiting for venus_security; see /skyward/run/state/recovery/security.log"
            );
            // ctl.start is asynchronous. Allow its first transition to running.
            ensure!(
                state.as_deref() != Some("stopped")
                    || started.elapsed() < std::time::Duration::from_secs(2),
                "venus_security exited during setup; see /skyward/run/state/recovery/runtime.log"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // The fast path only verifies the existing owned processes and their
        // readiness; startup is performed solely in the independent service.
        let status = std::process::Command::new("/skyward/runtime/rw-runtime")
            .status()
            .context("prepare bundled HAL runtime")?;
        ensure!(
            status.success(),
            "bundled HAL startup failed; see /skyward/run/state/recovery/runtime.log"
        );
        Ok(())
    }
    fn crypto_property(name: &str, value: &str) -> Result<()> {
        if skyward_sys::prop(name).as_deref() == Some(value) {
            return Ok(());
        }
        use std::os::unix::process::CommandExt;
        let status = std::process::Command::new("/system/bin/skyward")
            .arg0("resetprop")
            .args(["-n", name, value])
            .status()?;
        ensure!(
            status.success() && skyward_sys::prop(name).as_deref() == Some(value),
            "could not publish crypto state {name}"
        );
        Ok(())
    }
    pub fn open() -> Result<Opening> {
        // A worker never blocks the drawing thread. The early init supervisor
        // should be running in parallel already; log readiness separately
        // from the MetadataCrypt, filesystem and CE work that follows.
        let total_started = std::time::Instant::now();
        crate::venus_keymaster::phase("DATA_RW_SECURITY_WAIT_BEGIN");
        prepare_services()?;
        let security_ms = total_started.elapsed().as_millis();
        let _ = skyward_sys::kmsg(&format!(
            "VENUS_DATA_TIMING security_ready_ms={security_ms}"
        ));
        let _guard = lock()?;
        for (name, value) in [("ro.crypto.state", "encrypted"), ("ro.crypto.type", "file")] {
            if skyward_sys::prop(name).is_none() {
                crypto_property(name, value)?;
            }
        }
        let hw = Binder::hardware()?;
        let binder = Binder::recovery_crypto()?;
        let km = Keystore::open(&binder)?;
        let mounted = if Path::new(RECORD).exists() {
            verify()?;
            None
        } else {
            no_other_data_mount()?;
            let entry = vendor_entry()?;
            let opts = options(&entry)?;
            crate::venus_keymaster::phase("DATA_RW_METADATA_UNWRAP_BEGIN");
            let metadata_started = std::time::Instant::now();
            let encryption = skyward_crypto::encryption::Encryption::of(&entry)?;
            let ephemeral = skyward_crypto::recovery::metadata_storage_key(&km, &entry)?;
            let _ = skyward_sys::kmsg(&format!(
                "VENUS_DATA_TIMING metadata_unwrap_ms={}",
                metadata_started.elapsed().as_millis()
            ));
            crate::venus_keymaster::phase("DATA_RW_DM_CREATE_BEGIN");
            let mapping_started = std::time::Instant::now();
            let size = skyward_block::size_bytes(SOURCE)?;
            ensure!(
                size >= 4096 && size.is_multiple_of(4096),
                "unaligned userdata size"
            );
            let mut target =
                skyward_crypto::encryption::target(SOURCE, size / 512, &ephemeral, &encryption);
            let result = dm::create(MAPPING, std::slice::from_ref(&target), false);
            if let dm::Target::DefaultKey { key, .. } = &mut target {
                key.zeroize();
            }
            let mapped = result?;
            let _ = skyward_sys::kmsg(&format!(
                "VENUS_DATA_TIMING dm_create_ms={}",
                mapping_started.elapsed().as_millis()
            ));
            // Only remove a mapping that never mounted. Never run the old
            // diagnostic unmount path implicitly on a later validation error.
            let mut checked = None;
            let result = (|| -> Result<()> {
                crypto_property(
                    "ro.crypto.fs_crypto_blkdev",
                    mapped.to_str().context("invalid crypto device path")?,
                )?;
                ensure!(
                    !dm::readonly(MAPPING)?,
                    "writable mapping creation did not take"
                );
                ensure!(
                    skyward_block::probe::filesystem(&mapped)?.as_deref()
                        == Some(entry.filesystem.as_str()),
                    "decrypted filesystem does not match the effective fstab"
                );
                if entry.extras.iter().any(|flag| flag == "check") {
                    // Never skip the installed fstab check simply because it
                    // is slow: a dirty F2FS filesystem must be checked before
                    // we mount its decrypted mapping read/write.
                    crate::venus_keymaster::phase("DATA_RW_FSCK_BEGIN");
                    let fsck_started = std::time::Instant::now();
                    let args: Vec<&str> = if entry.filesystem == "f2fs" {
                        vec!["-a"]
                    } else {
                        vec!["-y"]
                    };
                    let output = std::process::Command::new("/skyward/runtime/crypto-launch")
                        .arg(
                            &entry
                                .filesystem
                                .replace("f2fs", "fsck.f2fs")
                                .replace("ext4", "e2fsck"),
                        )
                        .args(args)
                        .arg(&mapped)
                        .output()?;
                    let code = output
                        .status
                        .code()
                        .context("filesystem check ended by signal")?;
                    let _ = skyward_sys::kmsg(&format!(
                        "VENUS_DATA_TIMING fsck_ms={} fsck_status={code}",
                        fsck_started.elapsed().as_millis()
                    ));
                    let said = format!(
                        "{}{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    checked = Some(skyward_block::fsck::Checked { status: code, said });
                }
                let m = fs::symlink_metadata(MOUNT)?;
                ensure!(
                    m.is_dir() && !m.file_type().is_symlink(),
                    "unsafe /data mount point"
                );
                crate::venus_keymaster::phase("DATA_RW_MOUNT_BEGIN");
                let mount_started = std::time::Instant::now();
                let mount_result = mount_metadata(
                    || {
                        if mount::is_mount_point(Path::new(MOUNT))? {
                            return Ok(());
                        }
                        mount::mount(&mapped, Path::new(MOUNT), &entry.filesystem, &opts)
                    },
                    || std::thread::sleep(std::time::Duration::from_micros(500)),
                )
                .context("mount metadata-decrypted userdata read/write");
                let _ = skyward_sys::kmsg(&format!(
                    "VENUS_DATA_TIMING mount_ms={}",
                    mount_started.elapsed().as_millis()
                ));
                mount_result
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
                filesystem: entry.filesystem.clone(),
                checked,
            })
        };
        verify()?;
        let mut crypto = CRYPTO
            .lock()
            .map_err(|_| anyhow::anyhow!("crypto session owner failed"))?;
        if crypto.is_none() {
            let entry = vendor_entry()?;
            let mode = entry
                .extra("fileencryption")
                .context("missing file encryption options")?;
            let mut init = INITIALIZING
                .lock()
                .map_err(|_| anyhow::anyhow!("crypto initialization owner failed"))?;
            if init.is_none() {
                *init = Some(Initialization::new(mode)?);
            }
            let mut result = None;
            for retry in 0..3 {
                match CryptoSession::initialize(&km, &hw, init.as_mut().unwrap()) {
                    Ok(session) => {
                        *crypto = Some(session);
                        break;
                    }
                    Err(error) => {
                        result = Some(error);
                        if retry < 2 {
                            std::thread::sleep(std::time::Duration::from_millis(2));
                        }
                    }
                }
            }
            if crypto.is_none() {
                return Err(result.context("DE initialization did not run")?);
            }
        }
        let session = crypto.as_ref().context("crypto session unavailable")?;
        if session.owner_ready() {
            show_storage()?;
        }
        let _ = skyward_sys::kmsg(&format!(
            "VENUS_DATA_TIMING open_total_ms={} security_wait_ms={security_ms} closed_users={}",
            total_started.elapsed().as_millis(),
            session.closed.len()
        ));
        Ok(Opening {
            mounted,
            closed: session.closed.clone(),
        })
    }
    pub fn unlock(closed: &[Closed], user: u32, credential: &[u8]) -> Result<Vec<Closed>> {
        let _guard = lock()?;
        verify()?;
        ensure!(
            closed.iter().any(|c| c.user == user),
            "user is not awaiting unlock"
        );
        let binder = Binder::recovery_crypto()?;
        let hw = Binder::hardware()?;
        let keystore = Keystore::open(&binder)?;
        let mut crypto = CRYPTO
            .lock()
            .map_err(|_| anyhow::anyhow!("crypto session owner failed"))?;
        // A separate CLI process reconstructs policies without retrying
        // already installed user credentials.
        if crypto.is_none() {
            let entry = vendor_entry()?;
            let mut init = INITIALIZING
                .lock()
                .map_err(|_| anyhow::anyhow!("crypto initialization owner failed"))?;
            if init.is_none() {
                *init = Some(Initialization::new(
                    entry
                        .extra("fileencryption")
                        .context("missing file encryption options")?,
                )?);
            }
            *crypto = Some(CryptoSession::initialize(
                &keystore,
                &hw,
                init.as_mut().unwrap(),
            )?);
        }
        let session = crypto.as_mut().context("crypto session unavailable")?;
        let result = session.unlock(&keystore, &hw, user, credential)?;
        verify()?;
        if session.owner_ready() {
            show_storage()?;
        }
        println!("VENUS_RECOVERY_CE_UNLOCK_OK user={user} readonly=false storage_prepared=true");
        Ok(result)
    }
    /// Mapping-only maintenance contract: no mount, fsck or DE initialize.
    pub fn map_unmounted() -> Result<std::path::PathBuf> {
        prepare_services()?;
        let _guard = lock()?;
        no_other_data_mount()?;
        let binder = Binder::recovery_crypto()?;
        let keystore = Keystore::open(&binder)?;
        let entry = vendor_entry()?;
        let encryption = skyward_crypto::encryption::Encryption::of(&entry)?;
        let dir = skyward_crypto::encryption::key_dir(&entry)?;
        let raw =
            Zeroizing::new(StoredKey::load_recovery(&dir)?.unwrap(Unwrap::Hardware(&keystore))?);
        let key = if encryption.wrapped {
            keystore.convert_storage_key(&raw)?.key
        } else {
            raw
        };
        let device = skyward_block::fstab::device(&entry)?;
        let sectors = skyward_block::size_bytes(&device)? / 512;
        let mut target = skyward_crypto::encryption::target(&device, sectors, &key, &encryption);
        let result = dm::create(MAPPING, std::slice::from_ref(&target), false);
        if let dm::Target::DefaultKey { key, .. } = &mut target {
            key.zeroize();
        }
        result
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
        ensure!(
            skyward_sys::prop("init.svc.venus_ui").as_deref() == Some("stopped"),
            "stop venus_ui and its workers before closing Data"
        );
        ensure!(
            skyward_sys::prop("sys.usb.state").as_deref() == Some("adb")
                && skyward_sys::prop("sys.usb.ffs.mtp.ready").as_deref() != Some("1"),
            "turn off MTP and restore adb before closing Data"
        );
        release_for_maintenance()
    }
    pub fn release_for_maintenance() -> Result<()> {
        let _guard = lock()?;
        if !mount::is_mount_point(Path::new(MOUNT))? {
            ensure!(
                !Path::new(RECORD).exists(),
                "Data receipt remains without its mount"
            );
            return Ok(());
        }
        ensure!(
            skyward_sys::prop("sys.usb.ffs.mtp.ready").as_deref() != Some("1"),
            "stop MTP before releasing Data"
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
        close_steps(
            storage,
            || {
                crate::venus_keymaster::phase("DATA_RW_CLOSE_RADIO_QUIESCE_BEGIN");
                let guard = crate::venus_modem::efs_guard()?;
                crate::venus_keymaster::phase("DATA_RW_CLOSE_RADIO_QUIESCE_OK");
                Ok(guard)
            },
            |step| {
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
                            return Err(std::io::Error::last_os_error())
                                .context("sync writable Data");
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
                    CloseStep::Receipt => {
                        fs::remove_file(RECORD)?;
                        *CRYPTO
                            .lock()
                            .map_err(|_| anyhow::anyhow!("crypto session owner failed"))? = None;
                        *INITIALIZING
                            .lock()
                            .map_err(|_| anyhow::anyhow!("crypto initialization owner failed"))? =
                            None;
                    }
                }
                crate::venus_keymaster::phase(&format!("DATA_RW_CLOSE_{name}_OK"));
                Ok(())
            },
        )?;
        println!(
            "VENUS_DATA_RW_CLOSE_OK synced=true mounts_removed=true mapping_removed=true receipt_removed=true"
        );
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
        let at = Path::new("/skyward/run/hal/cache");
        let meta = match fs::symlink_metadata(at) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("inspect RAM runtime cache"),
        };
        ensure!(
            meta.is_dir() && meta.uid() == 0 && meta.mode() & 0o077 == 0,
            "RAM runtime cache must be a private root-owned directory"
        );
        let mut filesystem: libc::statfs = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { libc::statfs(c"/skyward/run/hal/cache".as_ptr(), &mut filesystem) } == 0
                && filesystem.f_type as u64 == 0x01021994,
            "runtime cache must remain tmpfs"
        );
        let mut receipt = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(at.join("ready"))?;
        let meta = receipt.metadata()?;
        ensure!(
            meta.is_file() && meta.uid() == 0 && meta.mode() & 0o022 == 0 && meta.len() < 128,
            "invalid runtime cache receipt"
        );
        let mut raw = String::new();
        receipt.read_to_string(&mut raw)?;
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        ensure!(
            raw == format!("venus-runtime-cache-v1\nboot={}\n", boot.trim()),
            "stale RAM runtime cache"
        );
        Ok(Some(at.to_owned()))
    }
    pub fn cached_file(path: &Path) -> Result<Option<std::path::PathBuf>> {
        if !enabled() {
            return Ok(None);
        }
        let Ok(relative) = path.strip_prefix("/mnt/skyward-ro") else {
            return Ok(None);
        };
        ensure!(
            relative
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "invalid cached runtime path"
        );
        Ok(cache_root()?.map(|root| root.join(relative)))
    }
    pub fn info(cfg: &skyward_config::DeviceConfig) -> Result<skyward_info::DeviceInfo> {
        let mut parts = Vec::new();
        let cache = cache_root()?;
        for base in [
            "system",
            "system_ext",
            "system_dlkm",
            "vendor",
            "vendor_dlkm",
            "odm",
            "odm_dlkm",
            "product",
            "mi_ext",
        ] {
            let at = Path::new("/mnt/skyward-ro").join(base);
            // Once cached, background readings must not reopen a block
            // source while the install worker is trying to release it.
            if let Some(at) = cache
                .as_ref()
                .map(|root| root.join(base))
                .filter(|at| at.is_dir())
            {
                parts.push((base.to_string(), at));
            } else if cache.is_none() && at.exists() && mount::is_mount_point(&at)? {
                parts.push((base.to_string(), at));
            }
        }
        skyward_info::collect_with_installed_props(
            cfg,
            skyward_info::installed_props_from_mounts(&parts)?,
        )
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_mount_uses_upstream_retry_budget_without_credential_work() {
        let mut attempts = 0;
        let mut pauses = 0;
        let result = mount_metadata(
            || {
                attempts += 1;
                anyhow::bail!("injected mount failure")
            },
            || pauses += 1,
        );
        assert!(result.is_err());
        assert_eq!((attempts, pauses), (12, 9));
        let mut attempts = 0;
        mount_metadata(
            || {
                attempts += 1;
                Ok(())
            },
            || panic!("must not pause after success"),
        )
        .unwrap();
        assert_eq!(attempts, 1);
    }
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
            let result = close_steps(
                true,
                || Ok(()),
                |step| {
                    seen.push(step);
                    ensure!(
                        step != expected[fail],
                        "injected sync/unmount/remove failure"
                    );
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert_eq!(seen, expected[..=fail]);
        }
    }
    #[test]
    fn close_locked_storage_still_syncs_and_removes_receipt_last() {
        let mut seen = Vec::new();
        close_steps(
            false,
            || Ok(()),
            |step| {
                seen.push(step);
                Ok(())
            },
        )
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
        let result = close_steps(
            true,
            || -> Result<()> { anyhow::bail!("radio still owns its NV helpers") },
            |step| {
                seen.push(step);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(seen.is_empty());
    }
    #[test]
    fn close_keeps_radio_excluded_until_success_or_aborted_cleanup() {
        use std::{cell::Cell, rc::Rc};
        struct Guard(Rc<Cell<bool>>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        for fail in [None, Some(CloseStep::Data)] {
            let held = Rc::new(Cell::new(false));
            let mut seen = Vec::new();
            let result = close_steps(
                true,
                || {
                    held.set(true);
                    Ok(Guard(held.clone()))
                },
                |step| {
                    assert!(held.get(), "radio exclusion released during {step:?}");
                    seen.push(step);
                    ensure!(Some(step) != fail, "injected teardown failure");
                    Ok(())
                },
            );
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
        assert!(data.contains("fsync_mode=nobarrier"));
        for x in [
            "norecovery",
            "disable_roll_forward",
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
        assert!(options(&e).is_ok());
        e.filesystem = "vfat".into();
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
        assert!(
            Session::decode(
                &s.encode()
                    .replace("venus-rw-session-v1", "venus-readonly-data-session-v1")
            )
            .is_err()
        );
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
