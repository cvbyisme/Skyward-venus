//! Explicit venus Keymaster diagnostics. No credential guesses, key-file
//! writes, fsck, formatting, BCB handling or automatic writable data mount.
use anyhow::{Context, Result, ensure};
use skyward_binder::driver::Binder;
use skyward_crypto::keymint::KeyMint;
use skyward_crypto::keystore::{StoredKey, Unwrap};
use std::path::Path;
use zeroize::Zeroizing;

pub fn run(
    metadata: bool,
    memory_upgrade: bool,
    data_ro: bool,
    de: bool,
    keep_data_ro: bool,
    allow_teardown_test: bool,
) -> Result<()> {
    if data_ro {
        crate::venus_data_ro::validate_probe_mode(keep_data_ro, allow_teardown_test, de)?;
    }
    ensure!(
        std::fs::read_to_string("/skyward/probe-entryfix.txt")?.starts_with("venus-entryfix-v1\n"),
        "requires venus Recovery"
    );
    phase("KEYMASTER_CLIENT_BEGIN");
    let binder = Binder::hardware()?;
    let service = KeyMint::open_hidl(&binder, memory_upgrade)?;
    let hardware = service.hardware()?;
    println!(
        "KEYMASTER_HIDL_READY version={} security_level={} name={:?} author={:?}",
        hardware.version, hardware.security_level, hardware.name, hardware.author
    );
    if !metadata {
        return Ok(());
    }
    let dir = Path::new("/metadata/vold/metadata_encryption/key");
    let before = key_fingerprint(dir)?;
    let result = (|| -> Result<()> {
        let key = StoredKey::load(dir).context("read existing metadata wrapping layout")?;
        let params = key.authorizations(&service)?;
        // Only number of parameters; never print SID, app binding or blobs.
        println!("KEYMASTER_CHARACTERISTICS_OK count={}", params.len());
        // Only public capabilities; never print SID, key blobs or application bindings.
        for (tag, value) in &params {
            let t = *tag as u32;
            if matches!(
                t,
                0x20000001
                    | 0x10000002
                    | 0x30000003
                    | 0x20000004
                    | 0x20000006
                    | 0x30000008
                    | 0x700001f7
                    | 0x300002c1
                    | 0x300002c2
                    | 0x300002ce
                    | 0x300002cf
            ) {
                println!("KEYMASTER_PUBLIC_AUTH tag={t:#010x} value={value}");
            }
        }
        phase("METADATA_UNWRAP_BEGIN");
        let raw = Zeroizing::new(
            key.unwrap(Unwrap::Hardware(&service))
                .context("unwrap metadata storage blob")?,
        );
        println!("KEYMASTER_METADATA_UNWRAP_OK bytes={}", raw.len());
        let result = service
            .ephemeral_storage_key(&raw)
            .context("convert wrapped storage key with legacy RAW export");
        let ephemeral = Zeroizing::new(result?);
        println!(
            "KEYMASTER_EPHEMERAL_OK bytes={}; material never logged or saved",
            ephemeral.len()
        );
        phase("METADATA_EPHEMERAL_READY");
        if data_ro {
            crate::venus_data_ro::verify(&service, &ephemeral, de, keep_data_ro)?;
        }
        Ok(())
    })();
    ensure!(
        before == key_fingerprint(dir)?,
        "metadata key files changed during diagnostic"
    );
    println!("KEYMASTER_ORIGINAL_KEY_FILES_UNCHANGED");
    phase(if result.is_ok() {
        "KEYMASTER_DIAGNOSTIC_COMPLETE"
    } else {
        "KEYMASTER_DIAGNOSTIC_FAILED"
    });
    result
}

fn key_fingerprint(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    use sha2::{Digest, Sha256};
    let mut out = Vec::new();
    for name in [
        "version",
        "encrypted_key",
        "keymaster_key_blob",
        "secdiscardable",
        "stretching",
    ] {
        let path = dir.join(name);
        match std::fs::read(path) {
            Ok(bytes) => out.push((name.to_owned(), Sha256::digest(&bytes).to_vec())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(out)
}

/// Best-effort stage evidence only: no keys, IDs, credentials or blob bytes.
/// Keep the last phase on Metadata as well as kmsg, so a USB disconnect is
/// not mistakenly treated as successful cleanup. Failure to log is not fatal.
pub(crate) fn phase(name: &str) {
    // Send the phase to the host before entering a potentially blocking
    // syscall. Persistence below remains useful if USB disappears.
    use std::io::Write;
    println!("KEYMASTER_PHASE {name}");
    let _ = std::io::stdout().flush();
    let _ = skyward_sys::kmsg(&format!("VENUS_KEYMASTER_PHASE {name}"));
    let save = || -> Result<()> {
        use std::fs::{self, OpenOptions};
        use std::io::Write;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let dir = Path::new("/metadata/skyward-probe");
        ensure!(
            skyward_block::mount::is_mount_point(Path::new("/metadata"))?,
            "metadata not mounted"
        );
        let m = fs::symlink_metadata(dir)?;
        ensure!(
            m.is_dir() && !m.file_type().is_symlink() && m.uid() == 0,
            "unexpected log directory"
        );
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .mode(0o600)
            .open(dir.join("keymaster41-last-phase.txt"))?;
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        writeln!(f, "boot_id={}\nphase={name}", boot.trim())?;
        f.sync_all()?;
        Ok(())
    };
    if let Err(e) = save() {
        eprintln!("KEYMASTER_PHASE_LOG_UNAVAILABLE: {e}");
    }
}
