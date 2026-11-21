//! Recovery FBE lifecycle corresponding to TeamWin's Decrypt/FsCrypt/KeyStorage.
//! The UI owns no keys. Kernel policies survive individual Worker requests.
//! Runtime files are private RAM; Android's on-disk key layout is unchanged.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]
use crate::user;
use anyhow::{Result, bail, ensure};
use std::path::{Path, PathBuf};

pub const TMP: &str = "/skyward/run/crypto/tmp";
pub const KEYSTORE_DB: &str = "/skyward/run/crypto/tmp/misc/keystore/persistent.sqlite";
const TOOL: &str = "/skyward/runtime/crypto-launch";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub contents: u8,
    pub filenames: u8,
    pub flags: u8,
    pub wrapped: bool,
}
impl Options {
    pub fn parse(mode: &str) -> Result<Self> {
        let parts: Vec<_> = mode.trim().split(':').collect();
        ensure!(
            (2..=3).contains(&parts.len()),
            "invalid file encryption options"
        );
        let contents = match parts[0] {
            "aes-256-xts" => 1,
            "adiantum" => 9,
            other => bail!("unsupported content cipher {other}"),
        };
        let filenames = match parts[1] {
            "aes-256-cts" => 4,
            "aes-256-heh" => 126,
            "adiantum" => 9,
            other => bail!("unsupported filename cipher {other}"),
        };
        let mut options = Self {
            contents,
            filenames,
            flags: 2,
            wrapped: false,
        };
        if contents == 9 {
            options.flags |= 4;
        }
        for flag in parts.get(2).unwrap_or(&"v2").split('+') {
            match flag {
                "v2" => (),
                "inlinecrypt_optimized" => options.flags |= 8,
                "emmc_optimized" => options.flags |= 16,
                "wrappedkey_v0" => options.wrapped = true,
                other => bail!("unsupported recovery fscrypt option {other}"),
            }
        }
        Ok(options)
    }
    fn policy(&self, id: [u8; 16]) -> [u8; 24] {
        let mut policy = [0; 24];
        policy[0] = 2;
        policy[1] = self.contents;
        policy[2] = self.filenames;
        policy[3] = self.flags;
        policy[8..].copy_from_slice(&id);
        policy
    }
    fn mode(&self) -> String {
        let content = if self.contents == 9 {
            "adiantum"
        } else {
            "aes-256-xts"
        };
        let filename = match self.filenames {
            9 => "adiantum",
            126 => "aes-256-heh",
            _ => "aes-256-cts",
        };
        format!(
            "{content}:{filename}:v2{}{}{}",
            if self.flags & 8 != 0 {
                "+inlinecrypt_optimized"
            } else {
                ""
            },
            if self.flags & 16 != 0 {
                "+emmc_optimized"
            } else {
                ""
            },
            if self.wrapped { "+wrappedkey_v0" } else { "" }
        )
    }
}

/// Decrypt.cpp: v2 derives a SHA-512 FBE secret; v3 uses SP800-108.
pub fn file_secret(blob: &[u8], sp: &[u8]) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    ensure!(
        blob.len() >= 2 && blob[1] == 0,
        "invalid credential SP blob"
    );
    Ok(zeroize::Zeroizing::new(match blob[0] {
        2 => user::personalized_hash(b"fbe-key", &[sp]),
        3 => user::file_secret(sp),
        other => bail!("unsupported synthetic password version {other}"),
    }))
}
fn candidates(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && entry.file_name().as_encoded_bytes().first() == Some(&b'c')
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    paths.reverse();
    Ok(paths)
}

#[cfg(target_os = "android")]
mod android {
    use super::*;
    use crate::fscrypt;
    use crate::{
        data::Closed,
        keystore::{StoredKey, Unwrap, hash_with_prefix},
        keystore2_client::Keystore,
    };
    use anyhow::Context;
    use skyward_binder::driver::Binder;
    use std::{
        collections::BTreeMap,
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
        process::Command,
    };
    use zeroize::{Zeroize, Zeroizing};

    fn phase(name: &str) {
        let _ = skyward_sys::kmsg(&format!("DATA_DECRYPT_PHASE {name}"));
    }
    fn tool(role: &str, args: &[&str]) -> Result<()> {
        let status = Command::new(TOOL)
            .arg(role)
            .args(args)
            .status()
            .with_context(|| format!("start crypto {role}"))?;
        ensure!(status.success(), "crypto {role} failed: {status}");
        Ok(())
    }
    fn write(path: &Path, bytes: &[u8]) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    }
    fn random(size: usize) -> Result<Zeroizing<Vec<u8>>> {
        let mut bytes = Zeroizing::new(vec![0; size]);
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    fn prepare_dir(path: &Path, mode: u32, uid: u32, gid: u32) -> Result<()> {
        fs::create_dir_all(path)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        ensure!(
            unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } == 0
                && unsafe { libc::fchmod(file.as_raw_fd(), mode) } == 0,
            "prepare storage directory {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
        Ok(())
    }
    fn ensure_policy(path: &Path, options: &Options, id: [u8; 16]) -> Result<()> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let policy = options.policy(id);
        if unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                0x800c6613u32 as libc::Ioctl,
                policy.as_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("set/verify fscrypt policy on {}", path.display()));
        }
        Ok(())
    }
    fn media_acl(path: &Path) -> Result<()> {
        // SetDefaultAcl(02770, media_rw, media_rw, {media_rw}).
        let mut acl = Vec::from(2u32.to_le_bytes());
        for (tag, perm, id) in [
            (1u16, 7u16, u32::MAX),
            (4, 7, u32::MAX),
            (8, 7, 1023),
            (16, 7, u32::MAX),
            (32, 0, u32::MAX),
        ] {
            acl.extend(tag.to_le_bytes());
            acl.extend(perm.to_le_bytes());
            acl.extend(id.to_le_bytes());
        }
        let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
        if unsafe {
            libc::setxattr(
                path.as_ptr(),
                c"system.posix_acl_default".as_ptr(),
                acl.as_ptr().cast(),
                acl.len(),
                0,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error()).context("set shared-storage default ACL");
        }
        Ok(())
    }
    fn prepare_storage(user: u32, ce: bool, options: &Options, id: [u8; 16]) -> Result<()> {
        let suffix = user.to_string();
        if !ce {
            prepare_dir(
                &Path::new("/data/system/users").join(&suffix),
                0o700,
                1000,
                1000,
            )?;
            prepare_dir(
                &Path::new("/data/misc/profiles/cur").join(&suffix),
                0o771,
                1000,
                1000,
            )?;
        }
        for (base, mode, uid, gid) in if ce {
            [
                ("system_ce", 0o770, 1000, 1000),
                ("misc_ce", 0o1771, 1000, 9998),
                ("vendor_ce", 0o771, 0, 0),
                ("user", 0o771, 1000, 1000),
            ]
        } else {
            [
                ("system_de", 0o770, 1000, 1000),
                ("misc_de", 0o1771, 1000, 9998),
                ("vendor_de", 0o771, 0, 0),
                ("user_de", 0o771, 1000, 1000),
            ]
        } {
            let path = if ce
                && base == "user"
                && user == 0
                && fs::symlink_metadata("/data/data").is_ok_and(|m| m.is_dir())
            {
                PathBuf::from("/data/data")
            } else {
                Path::new("/data").join(base).join(&suffix)
            };
            prepare_dir(&path, mode, uid, gid)?;
            ensure_policy(&path, options, id)?;
        }
        if ce {
            let media = Path::new("/data/media").join(&suffix);
            prepare_dir(&media, 0o2770, 1023, 1023)?;
            media_acl(&media)?;
            ensure_policy(&media, options, id)?;
            for base in ["system_ce", "vendor_ce", "misc_ce"] {
                // Utils::RestoreconRecursive delegates to init, which owns
                // the recovery's loaded SELinux file-context handle.
                let path = format!("/data/{base}/{user}");
                let restored = (|| -> Result<()> {
                    skyward_sys::set_prop("selinux.restorecon_recursive", "")?;
                    skyward_sys::set_prop("selinux.restorecon_recursive", &path)?;
                    skyward_sys::wait_prop(
                        "selinux.restorecon_recursive",
                        &path,
                        std::time::Duration::from_secs(5),
                    )
                })();
                if let Err(error) = restored {
                    let _ = skyward_sys::kmsg(&format!("storage restorecon: {error:#}"));
                }
            }
        }
        tool(
            "prepare-subdirs",
            &["prepare", "", &suffix, if ce { "2" } else { "1" }],
        )
    }
    fn storage_key(keystore: &Keystore<'_>, wrapped: bool) -> Result<Zeroizing<Vec<u8>>> {
        if wrapped {
            keystore.generate_storage_key()
        } else {
            random(64)
        }
    }
    fn install(keystore: &Keystore<'_>, raw: &[u8], options: &Options) -> Result<[u8; 16]> {
        if options.wrapped {
            // Keymaster::exportKey deliberately does not commit an upgraded
            // storage blob in this TeamWin branch.
            let key = keystore.convert_storage_key(raw)?.key;
            fscrypt::install_key(Path::new("/data"), &key, true)
        } else {
            fscrypt::install_key(Path::new("/data"), raw, false)
        }
    }
    pub fn metadata_storage_key(
        keystore: &Keystore<'_>,
        entry: &skyward_block::fstab::Entry,
    ) -> Result<Zeroizing<Vec<u8>>> {
        let encryption = crate::encryption::Encryption::of(entry)?;
        let directory = crate::encryption::key_dir(entry)?;
        if !directory.exists() {
            let parent = directory.parent().context("metadata key has no parent")?;
            prepare_dir(parent, 0o700, 0, 0)?;
            let key = if encryption.wrapped {
                keystore.generate_storage_key()?
            } else {
                random(if encryption.cipher == "aes-xts-plain64" {
                    64
                } else {
                    32
                })?
            };
            // This is the on-disk MetadataCrypt transaction directory,
            // intentionally distinct from Recovery's private RAM TMP.
            store(keystore, &directory, &parent.join("tmp"), &key)?;
        }
        let raw = load(keystore, &directory, &[])?;
        if encryption.wrapped {
            Ok(keystore.convert_storage_key(&raw)?.key)
        } else {
            Ok(raw)
        }
    }
    fn load(keystore: &Keystore<'_>, path: &Path, secret: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        let stored = StoredKey::load_recovery(path)?;
        Ok(Zeroizing::new(if secret.is_empty() {
            stored.unwrap(Unwrap::Hardware(keystore))?
        } else {
            stored.unwrap(Unwrap::<Keystore>::Secret(secret))?
        }))
    }
    fn store(keystore: &Keystore<'_>, directory: &Path, temp: &Path, key: &[u8]) -> Result<()> {
        ensure!(
            !directory.exists() && !temp.exists(),
            "key destination or transaction already exists"
        );
        prepare_dir(temp, 0o700, 0, 0)?;
        let secdis = random(16384)?;
        let app = Zeroizing::new(hash_with_prefix(b"Android secdiscardable SHA512", &secdis));
        let blob = keystore.generate_wrapping_key(&app)?;
        let (nonce, encrypted) = keystore.encrypt_vold(&blob, &app, key)?;
        let mut message = Zeroizing::new(nonce);
        message.extend_from_slice(&encrypted);
        for (name, bytes) in [
            ("version", &b"1"[..]),
            ("secdiscardable", &secdis[..]),
            ("keymaster_key_blob", &blob[..]),
            ("encrypted_key", &message[..]),
        ] {
            write(&temp.join(name), bytes)?;
        }
        File::open(temp)?.sync_all()?;
        fs::rename(temp, directory)?;
        File::open(directory.parent().context("key has no parent")?)?.sync_all()?;
        Ok(())
    }
    /// Same main-DB-only copy as Decrypt.cpp::copySqliteDb. No WAL or live
    /// database lookup bypasses the keystore2 service.
    pub fn copy_keystore_db() -> Result<()> {
        let directory = Path::new(KEYSTORE_DB).parent().expect("database parent");
        prepare_dir(directory, 0o700, 0, 0)?;
        let source = Path::new("/data/misc/keystore/persistent.sqlite");
        if source.is_file() {
            write(Path::new(KEYSTORE_DB), &fs::read(source)?)?;
        }
        Ok(())
    }
    fn fixate(keystore: &Keystore<'_>, directory: &Path, chosen: &Path, paths: &[PathBuf]) {
        for path in paths.iter().filter(|p| p.as_path() != chosen) {
            for name in ["keymaster_key_blob", "keymaster_key_blob_upgraded"] {
                if let Ok(mut blob) = fs::read(path.join(name)) {
                    let _ = keystore.delete_blob(&blob);
                    blob.zeroize();
                }
            }
            // Keep upstream's best-effort secure-discard step before rm.
            let _ = Command::new("/system/bin/secdiscard")
                .arg("--")
                .arg(path.join("encrypted_key"))
                .arg(path.join("secdiscardable"))
                .status();
            // As upstream, failure destroying an obsolete candidate does
            // not invalidate the successfully retrieved current key.
            if let Err(error) = fs::remove_dir_all(path) {
                let _ = skyward_sys::kmsg(&format!("CE candidate cleanup: {error}"));
            }
        }
        let current = directory.join("current");
        if chosen != current
            && let Err(error) = fs::rename(chosen, &current)
        {
            let _ = skyward_sys::kmsg(&format!("CE fixation rename: {error}"));
        }
        if let Ok(directory) = File::open(directory) {
            let _ = directory.sync_all();
        }
    }
    fn ce_key(keystore: &Keystore<'_>, user: u32, secret: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        let directory = PathBuf::from(format!("/data/misc/vold/user_keys/ce/{user}"));
        let paths = candidates(&directory)?;
        for path in &paths {
            if let Ok(key) = load(keystore, path, secret) {
                fixate(keystore, &directory, path, &paths);
                return Ok(key);
            }
        }
        bail!("no CE key candidate could be retrieved for user {user}")
    }
    struct Protector {
        id: String,
        blob: Zeroizing<Vec<u8>>,
        lock: Option<user::Lock>,
        weaver: Option<u32>,
    }
    impl Protector {
        fn load(user: u32) -> Result<Option<Self>> {
            let Some(id) = crate::locksettings::current_protector(
                Path::new(crate::locksettings::DATABASE),
                user,
            )?
            else {
                return Ok(None);
            };
            let prefix = PathBuf::from(format!("/data/system_de/{user}/spblob/{id}"));
            let with = |extension| prefix.with_extension(extension);
            let blob = Zeroizing::new(fs::read(with("spblob"))?);
            ensure!(
                blob.len() >= 30 && matches!(blob[0], 2 | 3) && blob[1] == 0,
                "unsupported or truncated SP blob"
            );
            let lock = if with("pwd").exists() {
                Some(user::Lock::of(&fs::read(with("pwd"))?)?)
            } else {
                None
            };
            let weaver = if with("weaver").exists() {
                Some(user::weaver_slot(&fs::read(with("weaver"))?)?)
            } else {
                None
            };
            Ok(Some(Self {
                id,
                blob,
                lock,
                weaver,
            }))
        }
        fn secret(
            &self,
            user: u32,
            credential: &[u8],
            default: bool,
            ks: &Keystore<'_>,
            hardware: &Binder,
        ) -> Result<Zeroizing<Vec<u8>>> {
            let stretched = Zeroizing::new(if default {
                user::stretched_without_credential()
            } else {
                self.lock
                    .as_ref()
                    .context("user has no password data")?
                    .stretched_recovery(credential)?
            });
            let application = if let Some(slot) = self.weaver {
                let weaver = crate::weaver::HidlWeaver::open(hardware)?;
                let config = weaver.config()?;
                let key = Zeroizing::new(user::weaver_key(&stretched, config.key_bytes)?);
                let value = Zeroizing::new(weaver.read(slot, &key)?);
                Zeroizing::new(user::weaver_protector_secret(&stretched, &value))
            } else {
                let secdis = Zeroizing::new(fs::read(format!(
                    "/data/system_de/{user}/spblob/{}.secdis",
                    self.id
                ))?);
                if !default {
                    let pwd = self
                        .lock
                        .as_ref()
                        .context("missing Gatekeeper password data")?;
                    let password = Zeroizing::new(user::gatekeeper_password(&stretched));
                    let token = crate::gatekeeper::Gatekeeper::open_hidl(hardware)?.verify(
                        100000 + user,
                        &pwd.enrolled,
                        &password,
                    )?;
                    ks.add_auth_token(&token)?;
                }
                Zeroizing::new(user::protector_secret(&stretched, &secdis))
            };
            let alias = format!("synthetic_password_{}", self.id.trim_start_matches('0'));
            let outer = ks.decrypt_alias(&alias, &self.blob[2..14], &self.blob[14..])?;
            let sp = Zeroizing::new(user::open_inner(&application, &outer)?);
            file_secret(&self.blob, &sp)
        }
    }
    impl Drop for Protector {
        fn drop(&mut self) {
            if let Some(lock) = &mut self.lock {
                lock.salt.zeroize();
                lock.enrolled.zeroize();
            }
        }
    }

    /// No Binder handles or raw keys live here. Policy identity and user
    /// storage readiness persist across Open/Unlock Worker invocations.
    pub struct Session {
        options: Options,
        de: BTreeMap<u32, [u8; 16]>,
        ce: BTreeMap<u32, [u8; 16]>,
        pub closed: Vec<Closed>,
    }
    /// The wrapped-mode retry is shared across DE attempts, like vold's
    /// mutable fstab entry and one-shot retry flag.
    pub struct Initialization {
        options: Options,
        may_toggle: bool,
    }
    impl Initialization {
        pub fn new(mode: &str) -> Result<Self> {
            Ok(Self {
                options: Options::parse(mode)?,
                may_toggle: true,
            })
        }
    }
    impl Session {
        pub fn initialize(
            ks: &Keystore<'_>,
            hardware: &Binder,
            init: &mut Initialization,
        ) -> Result<Self> {
            phase("DE_SYSTEM_BEGIN");
            // Missing session infrastructure is not a wrapped-key format
            // mismatch. Prepare it before the compatibility retry below.
            fscrypt::ensure_provisioning_keyring()?;
            let mut options = init.options.clone();
            let key_dir = Path::new("/data/unencrypted/key");
            if !key_dir.exists() {
                let key = storage_key(ks, options.wrapped)?;
                store(ks, key_dir, Path::new("/data/unencrypted/temp"), &key)?;
            }
            let raw = load(ks, key_dir, &[])?;
            let system = match install(ks, &raw, &options) {
                Ok(id) => id,
                Err(first) if init.may_toggle => {
                    init.may_toggle = false;
                    options.wrapped = !options.wrapped;
                    match install(ks, &raw, &options) {
                        Ok(id) => { init.options = options.clone(); id }
                        Err(second) => return Err(first.context(format!(
                            "system key installation failed; alternate wrapped mode also failed: {second:#}"
                        ))),
                    }
                }
                Err(error) => return Err(error),
            };
            write(
                Path::new("/data/unencrypted/mode"),
                options.mode().as_bytes(),
            )?;
            write(Path::new("/data/unencrypted/ref"), &system)?;
            // An existing current-boot receipt permits safe Worker/process
            // re-entry without generating another per-boot key.
            let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
            let receipt = Path::new("/skyward/run/state/per-boot-key");
            let reused = fs::read_to_string(receipt).ok().as_deref() == Some(boot.as_str())
                && fscrypt::recorded_name(Path::new("/data/unencrypted/per_boot_ref")).is_ok_and(
                    |id| fscrypt::reload_key(Path::new("/data"), id, options.wrapped).is_ok(),
                );
            if !reused {
                let key = storage_key(ks, options.wrapped)?;
                let id = install(ks, &key, &options)?;
                write(Path::new("/data/unencrypted/per_boot_ref"), &id)?;
                write(receipt, boot.as_bytes())?;
            }
            for path in [
                "/data/misc/vold/user_keys",
                "/data/misc/vold/user_keys/ce",
                "/data/misc/vold/user_keys/de",
            ] {
                prepare_dir(Path::new(path), 0o700, 0, 0)?;
            }
            let mut session = Self {
                options,
                de: BTreeMap::new(),
                ce: BTreeMap::new(),
                closed: Vec::new(),
            };
            if !Path::new("/data/misc/vold/user_keys/de/0").exists() {
                let key = storage_key(ks, session.options.wrapped)?;
                store(
                    ks,
                    Path::new("/data/misc/vold/user_keys/de/0"),
                    Path::new("/data/misc/vold/user_keys/temp"),
                    &key,
                )?;
                prepare_dir(Path::new("/data/misc/vold/user_keys/ce/0"), 0o700, 0, 0)?;
                let key = storage_key(ks, session.options.wrapped)?;
                store(
                    ks,
                    Path::new("/data/misc/vold/user_keys/ce/0/current"),
                    Path::new("/data/misc/vold/user_keys/temp"),
                    &key,
                )?;
            }
            for entry in fs::read_dir("/data/misc/vold/user_keys/de")? {
                let entry = entry?;
                let Some(id) = entry
                    .file_name()
                    .to_str()
                    .and_then(|v| v.parse::<u32>().ok())
                else {
                    continue;
                };
                let raw = load(ks, &entry.path(), &[])?;
                let policy = install(ks, &raw, &session.options)?;
                session.de.insert(id, policy);
            }
            let owner = *session.de.get(&0).context("missing owner DE policy")?;
            prepare_storage(0, false, &session.options, owner)?;
            phase("DE_READY");
            for user in session.de.keys().copied().collect::<Vec<_>>() {
                let protector = Protector::load(user)?;
                let kind = protector
                    .as_ref()
                    .and_then(|p| p.lock.as_ref())
                    .map_or(user::Lock::NONE, |p| p.kind);
                let path = PathBuf::from(format!("/data/system_ce/{user}"));
                if path.is_dir()
                    && let Some(id) = fscrypt::v2_policy_identifier(&path)?
                    && fscrypt::unlocked(&path)?
                {
                    session.ce.insert(user, id);
                    if let Err(error) = prepare_storage(user, true, &session.options, id) {
                        session.closed.push(Closed {
                            user,
                            lock: kind,
                            made_up: false,
                            why: Some(format!("{error:#}")),
                        });
                    }
                    continue;
                }
                session.closed.push(Closed {
                    user,
                    lock: kind,
                    made_up: false,
                    why: None,
                });
            }
            // Decrypt_Data first selects the owner's default-credential
            // branch. Only owner success drives other users' attempts.
            if (session.owner_ready()
                || session
                    .closed
                    .iter()
                    .any(|c| c.user == 0 && c.lock == user::Lock::NONE))
                && let Err(error) = session.unlock(ks, hardware, 0, b"!")
                && let Some(owner) = session.closed.iter_mut().find(|c| c.user == 0)
            {
                owner.why = Some(format!("{error:#}"));
            }
            if !session.owner_ready() {
                copy_keystore_db()?;
            }
            Ok(session)
        }
        fn unlock_one(
            &mut self,
            ks: &Keystore<'_>,
            hardware: &Binder,
            user: u32,
            credential: &[u8],
            default: bool,
        ) -> Result<()> {
            if let Some(id) = self.ce.get(&user).copied() {
                return prepare_storage(user, true, &self.options, id);
            }
            // A UI restart must not submit a credential for an already
            // installed CE policy. Recheck real kernel state, then prepare.
            let directory = PathBuf::from(format!("/data/system_ce/{user}"));
            if directory.is_dir()
                && let Some(id) = fscrypt::v2_policy_identifier(&directory)?
                && fscrypt::unlocked(&directory)?
            {
                self.ce.insert(user, id);
                return prepare_storage(user, true, &self.options, id);
            }
            phase("CE_CREDENTIAL_BEGIN");
            let secret = if let Some(protector) = Protector::load(user)? {
                if default {
                    copy_keystore_db()?;
                }
                protector.secret(user, credential, default, ks, hardware)?
            } else {
                ensure!(default, "user has no synthetic-password protector");
                Zeroizing::new(Vec::new())
            };
            let key = ce_key(ks, user, &secret)?;
            let id = install(ks, &key, &self.options)?;
            self.ce.insert(user, id);
            // Retain a failed prepare as a pending user, not StorageReady.
            prepare_storage(user, true, &self.options, id)?;
            phase("CE_STORAGE_READY");
            Ok(())
        }
        pub fn unlock(
            &mut self,
            ks: &Keystore<'_>,
            hardware: &Binder,
            user: u32,
            credential: &[u8],
        ) -> Result<Vec<Closed>> {
            self.unlock_one(ks, hardware, user, credential, credential == b"!")?;
            self.closed.retain(|c| c.user != user);
            if user == 0 {
                for other in self.closed.clone() {
                    let result =
                        self.unlock_one(ks, hardware, other.user, credential, credential == b"!");
                    let result = if result.is_err() && credential != b"!" {
                        self.unlock_one(ks, hardware, other.user, b"!", true)
                    } else {
                        result
                    };
                    match result {
                        Ok(()) => self.closed.retain(|c| c.user != other.user),
                        Err(error) => {
                            if let Some(c) = self.closed.iter_mut().find(|c| c.user == other.user) {
                                c.why = Some(format!("{error:#}"));
                            }
                        }
                    }
                }
            }
            Ok(self.closed.clone())
        }
        pub fn owner_ready(&self) -> bool {
            self.ce.contains_key(&0) && !self.closed.iter().any(|c| c.user == 0)
        }
    }
}
#[cfg(target_os = "android")]
pub use android::*;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sp_versions_use_different_reference_derivations() {
        let sp = b"recovery-test-synthetic-password";
        // Independent Python hashlib SHA-512 calculation for v2.
        assert_eq!(
            hex::encode(file_secret(&[2, 0], sp).unwrap().as_slice()),
            "1d57a867257f46e4ad9be2682426c9582627fdf5fe1b703d110e071408ae0d8dd04878652921b11d41ba29f2fa91260caa1da7dc541805a26f11b7f80d112437"
        );
        assert_eq!(&*file_secret(&[3, 0], sp).unwrap(), &user::file_secret(sp));
        assert!(file_secret(&[1, 0], sp).is_err());
        assert!(file_secret(&[3, 1], sp).is_err());
    }
    #[test]
    fn v2_policy_preserves_cipher_padding_and_inline_flags() {
        let mode = "aes-256-xts:aes-256-cts:v2+inlinecrypt_optimized+wrappedkey_v0";
        let options = Options::parse(mode).unwrap();
        let raw = options.policy([0x42; 16]);
        assert_eq!(&raw[..8], &[2, 1, 4, 10, 0, 0, 0, 0]);
        assert_eq!(&raw[8..], &[0x42; 16]);
        assert!(options.wrapped);
        assert_eq!(options.mode(), mode);
        assert!(Options::parse("aes-256-xts:aes-256-cts:v2+unknown").is_err());
    }
    #[test]
    fn ce_candidates_are_newest_first_and_exclude_foreign_paths() {
        let root = tempfile::tempdir().unwrap();
        for name in ["current", "cx0000000010", "cx0000000002", "unrelated"] {
            std::fs::create_dir(root.path().join(name)).unwrap();
        }
        std::fs::write(root.path().join("cx-file"), b"not a key directory").unwrap();
        std::os::unix::fs::symlink("unrelated", root.path().join("cx-symlink")).unwrap();
        let names: Vec<_> = candidates(root.path())
            .unwrap()
            .into_iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["cx0000000010", "cx0000000002", "current"]);
    }
}
