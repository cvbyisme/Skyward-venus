//! Owner CE unlock on the already-verified, retained read-only session.
//! No mounts, unmounts, key upgrades, enrollments or persistent credential
//! writes. A credential check is explicit, once, and never retried.
use anyhow::{Context, Result, ensure};
use skyward_binder::driver::Binder;
use skyward_crypto::{
    ce_store::{Profile, validate_input},
    fscrypt,
    gatekeeper::Gatekeeper,
    keymint::KeyMint,
    keystore::{StoredKey, Unwrap},
    user,
};
use std::{
    fs,
    io::{Read, Write},
    os::fd::AsRawFd,
    path::Path,
};
use zeroize::{Zeroize, Zeroizing};

const MOUNT: &str = "/mnt/skyward-data-ro";
const SOURCE: &str = "/dev/block/by-name/userdata";
const USER_SID: u32 = 0xa00001f6;

fn guard() -> Result<fs::File> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "CE entry requires Recovery root"
    );
    ensure!(
        fs::read_to_string("/skyward/probe-entryfix.txt")?.starts_with("venus-entryfix-v1\n"),
        "requires the temporary venus Recovery"
    );
    ensure!(
        !skyward_block::mount::is_mount_point(Path::new("/data"))?,
        "normal /data must remain untouched"
    );
    ensure!(
        crate::venus_data_session::reuse(SOURCE)?,
        "a verified retained Data session is required"
    );
    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open("/tmp/skyward-km41-data.lock")?;
    ensure!(
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another Data/CE operation is running"
    );
    ensure!(
        skyward_vab::snapshot_state()? == skyward_vab::SnapshotState::None,
        "OTA state is not known idle"
    );
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let node = fs::symlink_metadata("/dev/0:0:0:49476")
            .context("RPMB endpoint missing; no credential was read")?;
        ensure!(
            node.file_type().is_char_device()
                && node.uid() == 1000
                && node.gid() == 1000
                && node.mode() & 0o7777 == 0o600,
            "RPMB permissions do not match the installed Vendor rule (system:system 0600); run the updated local helper before credential input"
        );
        let id = fs::read_to_string("/sys/class/bsg/0:0:0:49476/dev")?;
        let expected = format!("{}:{}", libc::major(node.rdev()), libc::minor(node.rdev()));
        ensure!(
            id.trim() == expected,
            "RPMB endpoint identity mismatch; no credential was read"
        );
        println!("VENUS_CE_RPMB_DAC_OK owner=system:system mode=0600 identity=verified");
    }
    let de = Path::new(MOUNT).join("system_de/0");
    ensure!(fscrypt::unlocked(&de)?, "user0 DE must be unlocked first");
    ensure!(
        crate::venus_data_session::reuse(SOURCE)?,
        "retained Data identity changed"
    );
    // All disposable SQL copies must stay in tmpfs, never under userdata.
    let p = std::ffi::CString::new("/tmp").unwrap();
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { libc::statfs(p.as_ptr(), &mut st) } == 0 && st.f_type as u64 == 0x01021994,
        "CE scratch directory must be tmpfs"
    );
    Ok(f)
}

fn read_credential() -> Result<Zeroizing<Vec<u8>>> {
    ensure!(
        unsafe { libc::isatty(libc::STDIN_FILENO) } == 0,
        "use the local credential helper, not an echoing remote terminal"
    );
    let mut input = std::io::stdin().lock();
    let mut size = [0u8; 4];
    input.read_exact(&mut size)?;
    let size = u32::from_le_bytes(size) as usize;
    ensure!(size <= 4096, "credential frame is too large");
    let mut data = Zeroizing::new(vec![0; size]);
    input.read_exact(&mut data)?;
    let mut extra = [0u8; 1];
    ensure!(
        input.read(&mut extra)? == 0,
        "unexpected extra credential bytes"
    );
    Ok(data)
}

fn report_secret_error(stage: &str, error: anyhow::Error) -> anyhow::Error {
    // Hardware errors contain only protocol statuses, never credential bytes.
    error.context(stage.to_owned())
}

pub fn run(unlock_stdin: bool, accept_check: bool) -> Result<()> {
    ensure!(
        !unlock_stdin || accept_check,
        "local unlock requires explicit acknowledgement; no credential was read"
    );
    let _lock = guard()?;
    crate::venus_keymaster::phase("CE_PREFLIGHT_BEGIN");
    let hw = Binder::hardware()?;
    let keymaster = KeyMint::open_hidl(&hw, false)?; // No in-memory or on-disk upgrade.
    let gatekeeper = Gatekeeper::open_hidl(&hw)?;
    ensure!(
        gatekeeper.serves()? == skyward_crypto::gatekeeper::HARDWARE_SERVICE,
        "unexpected credential HAL descriptor"
    );
    println!(
        "GATEKEEPER_HIDL_READY interface=android.hardware.gatekeeper@1.0::IGatekeeper credential_submitted=false"
    );
    let profile = Profile::load(Path::new(MOUNT), Path::new("/tmp"))
        .context("read owner CE protector and wrapped-key layout")?;
    if let Some(l) = &profile.lock {
        let sid = u64::from_le_bytes(l.enrolled[1..9].try_into()?);
        ensure!(
            profile.cached_sid_matches(sid),
            "stored protector authorization does not match enrolled Gatekeeper SID"
        );
    }
    // Some QTI HALs refuse even characteristics for an authentication-bound
    // key until its user authenticates. Do not label that as key corruption,
    // and do not skip authentication: the real begin/finish still require HAT.
    match keymaster.authorizations(&profile.hardware_key, &[]) {
        Ok(params) => {
            if let Some(l) = &profile.lock {
                let sid = u64::from_le_bytes(l.enrolled[1..9].try_into()?);
                ensure!(
                    params
                        .iter()
                        .any(|(tag, value)| *tag as u32 == USER_SID && *value as u64 == sid),
                    "HAL protector authorization does not match enrolled Gatekeeper SID"
                );
            }
            println!("VENUS_CE_KEY_CHARACTERISTICS_OK");
        }
        Err(e)
            if skyward_binder::status::refused_with(&e) == Some(-26) && profile.lock.is_some() =>
        {
            println!(
                "VENUS_CE_KEY_REQUIRES_AUTHENTICATION code=-26 cached_sid_match=true; hardware_use_not_yet_verified"
            );
        }
        Err(e) => {
            return Err(e.context("read protector key characteristics before credential input"));
        }
    }
    let mode = fs::read_to_string(Path::new(MOUNT).join("unencrypted/mode"))?;
    ensure!(
        fscrypt::hardware_wrapped(mode.trim()),
        "unsupported CE storage-key form"
    );
    let media = Path::new(MOUNT).join("media/0");
    let _policy = fscrypt::v2_policy_identifier(&media)?
        .context("shared storage has no v2 encryption policy")?;
    let unlocked = fscrypt::unlocked(&media)?;
    profile.unchanged()?;
    println!(
        "VENUS_CE_PREFLIGHT_OK user=0 credential={} input_supported={} storage_unlocked={} source_files_unchanged=true",
        profile.kind(),
        profile.kind() != "pattern",
        unlocked
    );
    std::io::stdout().flush()?;
    if !unlock_stdin {
        crate::venus_keymaster::phase("CE_PREFLIGHT_COMPLETE");
        println!(
            "VENUS_CE_INSPECTION_COMPLETE; credential_submitted=false; teardown=not_attempted"
        );
        return Ok(());
    }
    ensure!(
        !unlocked,
        "CE is already unlocked; no credential will be checked again"
    );
    let credential = read_credential()?;
    validate_input(profile.kind(), &credential)?;
    let result = unlock_at(
        Path::new(MOUNT),
        &profile,
        &keymaster,
        &gatekeeper,
        &credential,
        || {
            ensure!(
                crate::venus_data_session::reuse(SOURCE)?,
                "retained Data identity changed"
            );
            Ok(())
        },
    );
    // Original protector files, database/WAL and vold wrapping files remain intact.
    profile.unchanged()?;
    println!("VENUS_CE_SOURCE_FILES_UNCHANGED");
    ensure!(
        crate::venus_data_session::reuse(SOURCE)?,
        "retained Data identity changed"
    );
    if result.is_ok() {
        crate::venus_keymaster::phase("CE_UNLOCK_COMPLETE");
        println!(
            "VENUS_CE_UNLOCK_OK user=0 shared_storage_unlocked=true readonly=true retained=true teardown=not_attempted"
        );
    } else {
        crate::venus_keymaster::phase("CE_UNLOCK_FAILED");
    }
    result
}

/// Shared, credential-authorized owner unlock implementation used by both the
/// original diagnostic CLI and the writable Recovery. The caller owns its
/// session lock and supplies a verifier; neither mode changes the other mode.
pub(crate) fn unlock_at(
    root: &Path,
    profile: &Profile,
    keymaster: &KeyMint<'_>,
    gatekeeper: &Gatekeeper<'_>,
    credential: &[u8],
    verify_session: impl Fn() -> Result<()>,
) -> Result<()> {
    ensure!(
        root == Path::new("/data") || root == Path::new(MOUNT),
        "unexpected owner data root"
    );
    verify_session()?;
    validate_input(profile.kind(), credential)?;
    ensure!(
        gatekeeper.serves()? == skyward_crypto::gatekeeper::HARDWARE_SERVICE,
        "unexpected credential service"
    );
    if let Some(l) = &profile.lock {
        let sid = u64::from_le_bytes(l.enrolled[1..9].try_into()?);
        ensure!(
            profile.cached_sid_matches(sid),
            "protector SID and enrolled handle disagree"
        );
    }
    ensure!(
        fscrypt::hardware_wrapped(fs::read_to_string(root.join("unencrypted/mode"))?.trim()),
        "unsupported CE key format"
    );
    let media = root.join("media/0");
    let policy =
        fscrypt::v2_policy_identifier(&media)?.context("owner media requires v2 policy")?;
    ensure!(
        !fscrypt::unlocked(&media)?,
        "CE already unlocked; refusing a redundant authentication"
    );
    let result = (|| -> Result<()> {
        let stretched = Zeroizing::new(match &profile.lock {
            Some(l) => l.stretched(&credential)?,
            None => user::stretched_without_credential(),
        });
        let mut token = match &profile.lock {
            Some(l) => {
                let pass = Zeroizing::new(user::gatekeeper_password(&stretched));
                crate::venus_keymaster::phase("CE_GATEKEEPER_VERIFY_BEGIN");
                // The sole LSKF verification; no alternate protectors or retries.
                let token = gatekeeper.verify(100000, &l.enrolled, &pass).map_err(|e| {
                    report_secret_error("owner credential verification failed; not retried", e)
                })?;
                let sid = u64::from_le_bytes(l.enrolled[1..9].try_into()?);
                ensure!(
                    token.user as u64 == sid && token.kind == 1 && token.challenge == 0,
                    "Gatekeeper auth token does not match protector"
                );
                // Keep the short-lived auth token on the fast path; do not
                // fsync Metadata between verification and Keymaster begin.
                println!("KEYMASTER_PHASE CE_GATEKEEPER_VERIFY_OK");
                Some(token)
            }
            None => None,
        };
        let secret = Zeroizing::new(user::protector_secret(&stretched, &profile.secdiscardable));
        let content = user::content(&profile.blob)?;
        let (nonce, body) = content.split_at(12);
        println!("KEYMASTER_PHASE CE_PROTECTOR_UNWRAP_BEGIN");
        let inner_result =
            keymaster.gcm_decrypt(&profile.hardware_key, &[], nonce, body, token.as_ref());
        if let Some(t) = &mut token {
            t.mac.zeroize();
        }
        let inner = Zeroizing::new(
            inner_result.context("unwrap credential-authorized protector; no retry or upgrade")?,
        );
        let sp = Zeroizing::new(user::open_inner(&secret, &inner)?);
        let file_secret = Zeroizing::new(user::file_secret(&sp));
        crate::venus_keymaster::phase("CE_PROTECTOR_UNWRAP_OK");
        let stored = StoredKey::load(&profile.ce_directory)?;
        let raw = Zeroizing::new(stored.unwrap(Unwrap::<KeyMint>::Secret(&file_secret))?);
        let ephemeral = Zeroizing::new(keymaster.ephemeral_storage_key(&raw)?);
        ensure!(
            ephemeral.len() == 68,
            "unexpected hardware-wrapped CE key length"
        );
        profile.unchanged()?;
        verify_session().context("Data identity changed before key install")?;
        crate::venus_keymaster::phase("CE_FSCRYPT_INSTALL_BEGIN");
        let installed = fscrypt::add_key(root, &ephemeral, true)?;
        ensure!(
            installed == policy,
            "CE key identifier does not match shared-storage policy"
        );
        ensure!(
            fscrypt::unlocked(&media)?,
            "CE key was not accepted for shared storage"
        );
        // Do not list or export any private filenames or file contents.
        crate::venus_keymaster::phase("CE_FSCRYPT_INSTALL_OK");
        Ok(())
    })();
    profile.unchanged()?;
    verify_session()?;
    result
}
