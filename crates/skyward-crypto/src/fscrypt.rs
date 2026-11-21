//! Letting a key into a filesystem, so the files it was used on can be
//! read. The kernel keeps the key for the filesystem, not for any one
//! file, and names it by a hash it works out itself.

use anyhow::{bail, Context, Result};
use std::path::Path;

/// How long the name the kernel gives a key is.
pub const NAME_BYTES: usize = 16;

/// Android init normally creates this keyring when it installs /data's
/// keys. Recovery mounts /data itself, so it must provide the same session
/// keyring before attempting any filesystem-key installation.
#[cfg(any(target_os = "android", test))]
fn find_or_create_keyring(
    search: impl FnOnce() -> std::io::Result<libc::c_long>,
    create: impl FnOnce() -> std::io::Result<libc::c_long>,
) -> std::io::Result<libc::c_long> {
    match search() {
        Err(error) if error.raw_os_error() == Some(libc::ENOKEY) => create(),
        result => result,
    }
}

#[cfg(target_os = "android")]
pub fn ensure_provisioning_keyring() -> Result<libc::c_long> {
    let checked = |result| {
        if result == -1 { Err(std::io::Error::last_os_error()) } else { Ok(result) }
    };
    find_or_create_keyring(
        || checked(unsafe {
            libc::syscall(libc::SYS_keyctl, 10 as libc::c_long, -3 as libc::c_long,
                c"keyring".as_ptr(), c"fscrypt".as_ptr(), 0 as libc::c_long)
        }),
        || checked(unsafe {
            libc::syscall(libc::SYS_add_key, c"keyring".as_ptr(), c"fscrypt".as_ptr(),
                std::ptr::null::<u8>(), 0usize, -3 as libc::c_long)
        }),
    ).context("prepare fscrypt session keyring")
}

/// KeyUtil::installProvisioningKey payload, shared by v2 fscrypt installs.
#[cfg(any(target_os = "android", test))]
fn provisioning_payload(raw: &[u8]) -> zeroize::Zeroizing<Vec<u8>> {
    let mut payload = zeroize::Zeroizing::new(vec![0; 8 + raw.len()]);
    payload[..4].copy_from_slice(&2u32.to_ne_bytes());
    payload[8..].copy_from_slice(raw);
    payload
}

/// TWRP installs both the filesystem key and a session provisioning key.
/// The latter permits reloading after a controlled unmount/remount.
#[cfg(target_os = "android")]
pub fn install_key(mount: &Path, raw: &[u8], wrapped: bool) -> Result<[u8; NAME_BYTES]> {
    let ring = ensure_provisioning_keyring()?;
    let identifier = add_key(mount, raw, wrapped)?;
    let name = std::ffi::CString::new(hex::encode(identifier))?;
    let payload = provisioning_payload(raw);
    if unsafe { libc::syscall(libc::SYS_add_key, c"fscrypt-provisioning".as_ptr(), name.as_ptr(),
            payload.as_ptr(), payload.len(), ring) } == -1 {
        return Err(std::io::Error::last_os_error()).context("install fscrypt provisioning key");
    }
    Ok(identifier)
}

/// KeyUtil::reloadKeyFromSessionKeyring for a known v2 policy. No hardware
/// authentication or key generation is performed on this path.
#[cfg(target_os = "android")]
pub fn reload_key(mount: &Path, id: [u8; NAME_BYTES], wrapped: bool) -> Result<()> {
    use std::os::fd::AsRawFd;
    let name = std::ffi::CString::new(hex::encode(id))?;
    let key = unsafe {libc::syscall(libc::SYS_keyctl,10 as libc::c_long,-3 as libc::c_long,
        c"fscrypt-provisioning".as_ptr(),name.as_ptr(),0 as libc::c_long)};
    if key == -1 { return Err(std::io::Error::last_os_error()).context("find session provisioning key"); }
    let mut arg = [0u8;80]; arg[..4].copy_from_slice(&2u32.to_ne_bytes());
    arg[8..24].copy_from_slice(&id);arg[44..48].copy_from_slice(&(key as u32).to_ne_bytes());
    if wrapped {arg[76..80].copy_from_slice(&1u32.to_ne_bytes());}
    let file = std::fs::File::open(mount)?;
    if unsafe {libc::ioctl(file.as_raw_fd(),0xc0506617u32 as libc::Ioctl,arg.as_mut_ptr())} != 0 {
        return Err(std::io::Error::last_os_error()).context("reload filesystem key from session keyring");
    }
    Ok(())
}

/// Let `raw` into the filesystem mounted at `mount`, and give the name the
/// kernel knows it by. Every file whose directory names that key can be
/// read from then on.
#[cfg(target_os = "android")]
pub fn add_key(mount: &Path, raw: &[u8], wrapped: bool) -> Result<[u8; NAME_BYTES]> {
    use std::os::fd::AsRawFd;
    /// The request's fixed part, before the key itself.
    const HEADER_BYTES: usize = 80;
    /// The key is named by what the kernel makes of it rather than by
    /// anything this side chooses.
    const BY_IDENTIFIER: u32 = 2;
    /// The key is one only the storage hardware can unwrap.
    const HARDWARE_WRAPPED: u32 = 1;
    const ADD_KEY: u32 = 0xc050_6617;

    let mut arg = zeroize::Zeroizing::new(vec![0u8; HEADER_BYTES + raw.len()]);
    arg[0..4].copy_from_slice(&BY_IDENTIFIER.to_ne_bytes());
    arg[40..44].copy_from_slice(&(raw.len() as u32).to_ne_bytes());
    if wrapped {
        arg[76..80].copy_from_slice(&HARDWARE_WRAPPED.to_ne_bytes());
    }
    arg[HEADER_BYTES..].copy_from_slice(raw);
    let dir = std::fs::File::open(mount).with_context(|| format!("open {}", mount.display()))?;
    if unsafe { libc::ioctl(dir.as_raw_fd(), ADD_KEY as libc::Ioctl, arg.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("let a key into {}", mount.display()));
    }
    let mut name = [0u8; NAME_BYTES];
    name.copy_from_slice(&arg[8..8 + NAME_BYTES]);
    Ok(name)
}

/// Whether the names and contents under `dir` read as they are: it is not
/// encrypted, or the key it is encrypted with is in the kernel. A user's
/// own files read only once that user's key is let in.
#[cfg(target_os = "android")]
pub fn unlocked(dir: &Path) -> Result<bool> {
    use std::os::fd::AsRawFd;
    const GET_POLICY: u32 = 0xc009_6616;
    const GET_KEY_STATUS: u32 = 0xc080_661a;
    /// How the first kind of policy names its key, and how the second does.
    const BY_DESCRIPTOR: u32 = 1;
    const BY_IDENTIFIER: u32 = 2;
    /// The second kind of policy is the largest: its version, three bytes
    /// of how it encrypts, four reserved, and the key's name.
    const POLICY_BYTES: usize = 24;
    const PRESENT: u32 = 2;

    let folder = std::fs::File::open(dir).with_context(|| format!("open {}", dir.display()))?;
    let mut policy = [0u8; 8 + POLICY_BYTES];
    policy[..8].copy_from_slice(&(POLICY_BYTES as u64).to_ne_bytes());
    if unsafe { libc::ioctl(folder.as_raw_fd(), GET_POLICY as libc::Ioctl, policy.as_mut_ptr()) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ENODATA) {
            return Ok(true);
        }
        return Err(e).with_context(|| format!("read how {} is encrypted", dir.display()));
    }
    let mut status = [0u8; 128];
    match policy[8] {
        0 => {
            status[..4].copy_from_slice(&BY_DESCRIPTOR.to_ne_bytes());
            status[8..16].copy_from_slice(&policy[12..20]);
        }
        2 => {
            status[..4].copy_from_slice(&BY_IDENTIFIER.to_ne_bytes());
            status[8..8 + NAME_BYTES].copy_from_slice(&policy[16..16 + NAME_BYTES]);
        }
        other => bail!("{} is encrypted under a policy of version {other}, which is not known here", dir.display()),
    }
    if unsafe { libc::ioctl(folder.as_raw_fd(), GET_KEY_STATUS as libc::Ioctl, status.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("ask whether the key for {} is in", dir.display()));
    }
    Ok(u32::from_ne_bytes(status[64..68].try_into().expect("four bytes")) == PRESENT)
}

/// Inspect a v2 policy identifier without adding keys or reading filenames.
#[cfg(target_os = "android")]
pub fn v2_policy_identifier(dir: &Path) -> Result<Option<[u8; NAME_BYTES]>> {
    use std::os::fd::AsRawFd;
    let f=std::fs::File::open(dir)?;
    let mut p=[0u8;32];p[..8].copy_from_slice(&24u64.to_ne_bytes());
    if unsafe{libc::ioctl(f.as_raw_fd(),0xc009_6616u32 as libc::Ioctl,p.as_mut_ptr())} != 0 {
        let e=std::io::Error::last_os_error();
        if e.raw_os_error()==Some(libc::ENODATA){return Ok(None);}
        return Err(e).context("query CE directory encryption policy");
    }
    anyhow::ensure!(p[8]==2,"CE directory does not use fscrypt v2");
    let mut id=[0;NAME_BYTES];id.copy_from_slice(&p[16..32]);Ok(Some(id))
}

/// Whether what a phone says about how it encrypts its files means the
/// keys are ones only the storage hardware can unwrap.
pub fn hardware_wrapped(mode: &str) -> bool {
    mode.split(['+', ':']).any(|part| part == "wrappedkey_v0")
}

/// Read the name the phone recorded for a key it let in, to check against
/// the name the kernel works out now.
pub fn recorded_name(path: &Path) -> Result<[u8; NAME_BYTES]> {
    let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    match <[u8; NAME_BYTES]>::try_from(&raw[..]) {
        Ok(name) => Ok(name),
        Err(_) => bail!("{} holds {} bytes, and a key's name is {NAME_BYTES}", path.display(), raw.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_creates_only_a_missing_session_keyring() {
        assert_eq!(find_or_create_keyring(|| Ok(17), || panic!("must reuse the inherited ring")).unwrap(), 17);
        assert_eq!(find_or_create_keyring(
            || Err(std::io::Error::from_raw_os_error(libc::ENOKEY)), || Ok(23),
        ).unwrap(), 23);
        for code in [libc::EACCES, libc::EPERM, libc::EKEYREVOKED] {
            assert_eq!(find_or_create_keyring(
                || Err(std::io::Error::from_raw_os_error(code)),
                || panic!("must not replace an inaccessible or revoked ring"),
            ).unwrap_err().raw_os_error(), Some(code));
        }
        assert_eq!(find_or_create_keyring(
            || Err(std::io::Error::from_raw_os_error(libc::ENOKEY)),
            || Err(std::io::Error::from_raw_os_error(libc::EDQUOT)),
        ).unwrap_err().raw_os_error(), Some(libc::EDQUOT));
    }

    #[test]
    fn provisioning_payload_has_v2_type_reserved_word_and_full_wrapped_key() {
        let payload = provisioning_payload(&[0x42;68]);
        assert_eq!(&payload[..8], &[2,0,0,0,0,0,0,0]);
        assert_eq!(&payload[8..], &[0x42;68]);
    }

    #[test]
    fn a_phone_says_whether_the_hardware_holds_the_keys() {
        assert!(hardware_wrapped("aes-256-xts:aes-256-cts:v2+inlinecrypt_optimized+wrappedkey_v0"));
        assert!(!hardware_wrapped("aes-256-xts:aes-256-cts:v2"));
        assert!(!hardware_wrapped("aes-256-xts:aes-256-cts:v2+wrappedkey_v1"));
    }

    #[test]
    fn a_name_is_read_back_only_at_its_own_length() {
        let dir = std::env::temp_dir().join(format!("skyward-fscrypt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("ref");
        std::fs::write(&good, [7u8; NAME_BYTES]).unwrap();
        assert_eq!(recorded_name(&good).unwrap(), [7u8; NAME_BYTES]);
        let short = dir.join("short");
        std::fs::write(&short, [7u8; 8]).unwrap();
        assert!(recorded_name(&short).is_err());
    }
}
