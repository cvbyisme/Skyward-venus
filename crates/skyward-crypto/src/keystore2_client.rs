//! Keystore 2.0 interfaces used by TWRP's Keymaster.cpp and Decrypt.cpp.
//! Wire definitions: AOSP android-12.1.0_r27 system/hardware/interfaces,
//! keystore2/aidl, and system/security/keystore2/aidl/authorization.
//! Database inspection in `keystore2` is deliberately a separate API.
use crate::token::Token;
use anyhow::{Result, ensure};
use skyward_binder::parcel::{Parcel, Reader};
#[cfg(target_os = "android")]
use zeroize::Zeroize;
use zeroize::Zeroizing;

pub const SERVICE: &str = "android.system.keystore2.IKeystoreService/default";
pub const AUTHORIZATION: &str = "android.security.authorization";
pub const KEYSTORE: &str = "android.system.keystore2.IKeystoreService";
pub const LEVEL: &str = "android.system.keystore2.IKeystoreSecurityLevel";
pub const OPERATION: &str = "android.system.keystore2.IKeystoreOperation";
pub const AUTH: &str = "android.security.authorization.IKeystoreAuthorization";
pub const VOLD_NAMESPACE: i64 = 100;
pub const LOCKSETTINGS_NAMESPACE: i64 = 103;

/// AIDL Domain::BLOB for vold, Domain::SELINUX for locksettings, or the
/// KEY_ID descriptor returned by getKeyEntry. Never log a descriptor.
pub struct Descriptor {
    pub domain: i32,
    pub namespace: i64,
    pub alias: Option<String>,
    pub blob: Option<Zeroizing<Vec<u8>>>,
}
impl Descriptor {
    pub fn vold(blob: &[u8]) -> Self {
        Self {
            domain: 3,
            namespace: VOLD_NAMESPACE,
            alias: None,
            blob: Some(Zeroizing::new(blob.to_vec())),
        }
    }
    pub fn locksettings(alias: &str) -> Self {
        Self {
            domain: 2,
            namespace: LOCKSETTINGS_NAMESPACE,
            alias: Some(alias.into()),
            blob: None,
        }
    }
    fn write(&self, p: &mut Parcel) {
        structure(p, |p| {
            p.i32(self.domain);
            p.i64(self.namespace);
            p.opt_string(self.alias.as_deref());
            p.opt_blob(self.blob.as_deref().map(|v| &v[..]));
        });
    }
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        let end = r.structure()?;
        let result = Self {
            domain: r.i32()?,
            namespace: r.i64()?,
            alias: r.opt_string()?,
            blob: r.opt_blob()?.map(Zeroizing::new),
        };
        ensure!(
            r.here() <= end,
            "keystore descriptor exceeds its parcelable"
        );
        r.go(end)?;
        Ok(result)
    }
}
/// Keep remote exception text out of credential errors. Some platform
/// implementations attach KeyDescriptor debug output to these messages.
fn checked(read: &mut Reader<'_>) -> Result<()> {
    skyward_binder::status::ok(read).map_err(|error| {
        match skyward_binder::status::refused_with(&error) {
            Some(code) => anyhow::anyhow!("keystore2 service refused operation: code={code}"),
            None => anyhow::anyhow!("keystore2 returned an invalid or rejected status"),
        }
    })
}

fn structure(p: &mut Parcel, fields: impl FnOnce(&mut Parcel)) {
    p.i32(1);
    let at = p.here();
    p.i32(0);
    fields(p);
    p.fill(at, (p.here() - at) as i32);
}
fn enum_parameter(p: &mut Parcel, tag: i32, arm: i32, value: i32) {
    structure(p, |p| {
        p.i32(tag);
        p.i32(1);
        p.i32(arm);
        p.i32(value);
    });
}
fn blob_parameter(p: &mut Parcel, tag: i32, value: &[u8]) {
    structure(p, |p| {
        p.i32(tag);
        p.i32(1);
        p.i32(14);
        p.blob(value);
    });
}
/// Parameters in the TWRP SP operation; vold adds APPLICATION_ID only when
/// it has one. KeyParameterValue arms are fixed by the KeyMint AIDL union.
fn gcm_parameters(p: &mut Parcel, app_id: &[u8], nonce: &[u8], encrypt: bool) {
    p.i32(5 + i32::from(!app_id.is_empty()) + i32::from(!nonce.is_empty()));
    enum_parameter(p, 0x10000002, 1, 32); // ALGORITHM/AES
    enum_parameter(p, 0x20000004, 2, 32); // BLOCK_MODE/GCM
    enum_parameter(p, 0x20000006, 3, 1); // PADDING/NONE
    enum_parameter(p, 0x20000001, 7, if encrypt { 0 } else { 1 });
    if !nonce.is_empty() {
        blob_parameter(p, 0x900003e9u32 as i32, nonce);
    }
    enum_parameter(p, 0x300003eb, 11, 128); // MAC_LENGTH/integer
    if !app_id.is_empty() {
        blob_parameter(p, 0x90000259u32 as i32, app_id);
    }
}
fn auth_token(p: &mut Parcel, token: &Token) {
    structure(p, |p| {
        p.i64(token.challenge);
        p.i64(token.user);
        p.i64(token.authenticator);
        p.i32(token.kind);
        structure(p, |p| p.i64(token.when));
        p.blob(&token.mac);
    });
}

#[cfg(target_os = "android")]
mod android {
    use super::*;
    use anyhow::Context;
    use skyward_binder::{
        driver::{Binder, Handle},
        service,
    };
    /// Actual keystore2 Binder client, never a direct KeyMint HAL client.
    pub struct Keystore<'a> {
        binder: &'a Binder,
        service: Handle<'a>,
        tee: Handle<'a>,
    }
    pub struct Entry<'a> {
        pub key: Descriptor,
        pub level: Handle<'a>,
    }
    pub struct EphemeralKey {
        pub key: Zeroizing<Vec<u8>>,
        pub upgraded_blob: Option<Zeroizing<Vec<u8>>>,
    }
    pub struct Operation<'a> {
        handle: Handle<'a>,
        live: bool,
        pub nonce: Option<Vec<u8>>,
        /// The caller implements KeyStorage's upgrade write/commit semantics.
        pub upgraded_blob: Option<Zeroizing<Vec<u8>>>,
    }
    impl<'a> Keystore<'a> {
        pub fn open(binder: &'a Binder) -> Result<Self> {
            let service = service::find(binder, SERVICE)?.context("keystore2 is not registered")?;
            let mut call = Parcel::call(KEYSTORE);
            call.i32(1); // SecurityLevel::TRUSTED_ENVIRONMENT, not software fallback.
            let reply = service.transact(1, &call)?;
            let mut r = reply.read();
            checked(&mut r)?;
            let tee = Handle::hold(
                binder,
                r.object()?
                    .context("keystore2 returned no TEE security level")?,
            )?;
            Ok(Self {
                binder,
                service,
                tee,
            })
        }
        pub fn add_auth_token(&self, token: &Token) -> Result<()> {
            ensure!(
                token.mac.len() == 32,
                "invalid hardware auth token MAC length"
            );
            let authorization = service::find(self.binder, AUTHORIZATION)?
                .context("keystore authorization is not registered")?;
            let mut call = Parcel::call(AUTH);
            auth_token(&mut call, token);
            let reply = authorization
                .transact(1, &call)
                .context("keystore authorization addAuthToken")?;
            checked(&mut reply.read())
        }
        pub fn key_entry(&self, key: &Descriptor) -> Result<Entry<'a>> {
            let mut call = Parcel::call(KEYSTORE);
            key.write(&mut call);
            let reply = self
                .service
                .transact(2, &call)
                .context("keystore2 getKeyEntry")?;
            let mut r = reply.read();
            checked(&mut r)?;
            let end = r.structure()?;
            let level = Handle::hold(
                self.binder,
                r.object()?.context("key has no security level")?,
            )?;
            let metadata_end = r.structure()?;
            let key = Descriptor::read(&mut r)?;
            ensure!(
                r.here() <= metadata_end && metadata_end <= end,
                "invalid keystore key metadata"
            );
            // Remaining metadata (authorizations, certificates, modification
            // time) is not needed for the SP operation. Do not decode blobs.
            r.go(end)?;
            Ok(Entry { key, level })
        }
        fn begin_on(
            &self,
            level: &Handle<'a>,
            key: &Descriptor,
            app: &[u8],
            nonce: &[u8],
            encrypt: bool,
        ) -> Result<Operation<'a>> {
            ensure!(
                nonce.len() == 12 || (encrypt && nonce.is_empty()),
                "AES-GCM requires a 12-byte nonce"
            );
            let mut call = Parcel::call(LEVEL);
            key.write(&mut call);
            gcm_parameters(&mut call, app, nonce, encrypt);
            call.bool(false);
            let reply = level
                .transact(1, &call)
                .context("keystore2 createOperation")?;
            let mut r = reply.read();
            checked(&mut r)?;
            let end = r.structure()?;
            let handle = Handle::hold(
                self.binder,
                r.object()?.context("keystore2 returned no operation")?,
            )?;
            // Guard immediately: malformed later fields must also abort it.
            let mut operation = Operation {
                handle,
                live: true,
                nonce: None,
                upgraded_blob: None,
            };
            if r.i32()? != 0 {
                // nullable OperationChallenge
                let at = r.here();
                r.go(at - 4)?;
                let next = r.structure()?;
                r.go(next)?;
            }
            if r.i32()? != 0 {
                // nullable KeyParameters
                let at = r.here();
                r.go(at - 4)?;
                let next = r.structure()?;
                let count = r.i32()?;
                ensure!(
                    (0..=256).contains(&count),
                    "invalid key operation parameter count"
                );
                for _ in 0..count {
                    let param_end = r.structure()?;
                    let tag = r.i32()?;
                    if tag == 0x900003e9u32 as i32 {
                        ensure!(r.i32()? != 0 && r.i32()? == 14, "invalid nonce parameter");
                        operation.nonce = Some(r.blob()?);
                    }
                    ensure!(
                        r.here() <= param_end && param_end <= next,
                        "invalid operation parameter"
                    );
                    r.go(param_end)?;
                }
                r.go(next)?;
            }
            operation.upgraded_blob = r.opt_blob()?.map(Zeroizing::new);
            ensure!(r.here() <= end, "invalid keystore operation response");
            r.go(end)?;
            Ok(operation)
        }
        pub fn begin_vold(&self, blob: &[u8], app: &[u8], nonce: &[u8]) -> Result<Operation<'a>> {
            self.begin_on(&self.tee, &Descriptor::vold(blob), app, nonce, false)
        }
        pub fn decrypt_alias(
            &self,
            alias: &str,
            nonce: &[u8],
            body: &[u8],
        ) -> Result<Zeroizing<Vec<u8>>> {
            let entry = self.key_entry(&Descriptor::locksettings(alias))?;
            let mut op = self.begin_on(&entry.level, &entry.key, &[], nonce, false)?;
            op.decrypt(body)
        }
        pub fn convert_storage_key(
            &self,
            blob: &[u8],
        ) -> Result<EphemeralKey> {
            let mut call = Parcel::call(LEVEL);
            Descriptor::vold(blob).write(&mut call);
            let reply = self
                .tee
                .transact(5, &call)
                .context("keystore2 convertStorageKeyToEphemeral")?;
            let mut r = reply.read();
            checked(&mut r)?;
            let end = r.structure()?;
            let key = Zeroizing::new(r.blob()?);
            let upgraded = r.opt_blob()?.map(Zeroizing::new);
            ensure!(r.here() <= end, "invalid ephemeral storage key response");
            r.go(end)?;
            Ok(EphemeralKey { key, upgraded_blob: upgraded })
        }
        pub fn delete_blob(&self, blob: &[u8]) -> Result<()> {
            let mut call = Parcel::call(LEVEL);
            Descriptor::vold(blob).write(&mut call);
            let reply = self.tee.transact(6, &call)?;
            checked(&mut reply.read())
        }
        pub fn encrypt_vold(
            &self,
            blob: &[u8],
            app: &[u8],
            body: &[u8],
        ) -> Result<(Vec<u8>, Zeroizing<Vec<u8>>)> {
            let mut op = self.begin_on(&self.tee, &Descriptor::vold(blob), app, &[], true)?;
            let nonce = op
                .nonce
                .take()
                .context("keystore encryption returned no nonce")?;
            ensure!(nonce.len() == 12, "invalid generated nonce length");
            Ok((nonce, op.decrypt(body)?))
        }
        pub fn generate_wrapping_key(&self, app: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
            self.generate_key(|p, rollback| {
                p.i32(if rollback { 10 } else { 9 });
                enum_parameter(p, 0x10000002, 1, 32);
                enum_parameter(p, 0x30000003, 11, 256);
                enum_parameter(p, 0x20000001, 7, 0);
                enum_parameter(p, 0x20000001, 7, 1);
                enum_parameter(p, 0x20000004, 2, 32);
                enum_parameter(p, 0x20000006, 3, 1);
                enum_parameter(p, 0x30000008, 11, 128); // MIN_MAC_LENGTH
                blob_parameter(p, 0x90000259u32 as i32, app);
                enum_parameter(p, 0x700001f7, 10, 1); // NO_AUTH_REQUIRED
                if rollback {
                    enum_parameter(p, 0x7000012f, 10, 1);
                }
            })
        }
        fn generate_key(&self, params: impl Fn(&mut Parcel, bool)) -> Result<Zeroizing<Vec<u8>>> {
            let attempt = |rollback| -> Result<Zeroizing<Vec<u8>>> {
                let key = Descriptor {
                    domain: 3,
                    namespace: VOLD_NAMESPACE,
                    alias: None,
                    blob: None,
                };
                let mut call = Parcel::call(LEVEL);
                key.write(&mut call);
                call.none();
                params(&mut call, rollback);
                call.i32(0);
                call.blob(&[]);
                let reply = self.tee.transact(2, &call)?;
                let mut r = reply.read();
                checked(&mut r)?;
                let end = r.structure()?;
                let mut key = Descriptor::read(&mut r)?;
                ensure!(r.here() <= end, "invalid generated key metadata");
                key.blob.take().context("keystore2 generated no key blob")
            };
            attempt(true).or_else(|_| attempt(false))
        }
        fn generate_storage_key_attempt(&self, rollback: bool) -> Result<Zeroizing<Vec<u8>>> {
            let key = Descriptor {
                domain: 3,
                namespace: VOLD_NAMESPACE,
                alias: None,
                blob: None,
            };
            let mut call = Parcel::call(LEVEL);
            key.write(&mut call);
            call.none();
            // KeyStorage::generateWrappedStorageKey: AesEncryptionKey(256),
            // STORAGE_KEY, Qualcomm FBE_ICE, then rollback resistance.
            call.i32(if rollback { 7 } else { 6 });
            enum_parameter(&mut call, 0x10000002, 1, 32);
            enum_parameter(&mut call, 0x30000003, 11, 256);
            enum_parameter(&mut call, 0x20000001, 7, 0);
            enum_parameter(&mut call, 0x20000001, 7, 1);
            enum_parameter(&mut call, 0x700002d2, 10, 1); // STORAGE_KEY = 722
            enum_parameter(&mut call, 0x70003f49, 10, 1); // FBE_ICE = 16201
            if rollback {
                enum_parameter(&mut call, 0x7000012f, 10, 1);
            }
            call.i32(0);
            call.blob(&[]);
            let reply = self.tee.transact(2, &call)?;
            let mut r = reply.read();
            checked(&mut r)?;
            let end = r.structure()?;
            let mut generated = Descriptor::read(&mut r)?;
            ensure!(r.here() <= end, "invalid generated key metadata");
            generated
                .blob
                .take()
                .context("keystore2 generated no storage key blob")
        }
        pub fn generate_storage_key(&self) -> Result<Zeroizing<Vec<u8>>> {
            // This is the upstream rollback-resistance fallback, not a
            // credential retry and not a direct hardware-service fallback.
            self.generate_storage_key_attempt(true)
                .or_else(|_| self.generate_storage_key_attempt(false))
        }
    }
    impl crate::keystore::Decryptor for Keystore<'_> {
        fn gcm_decrypt(
            &self,
            key: &[u8],
            app: &[u8],
            nonce: &[u8],
            body: &[u8],
        ) -> Result<Vec<u8>> {
            let mut operation = self.begin_vold(key, app, nonce)?;
            if let Some(blob) = &operation.upgraded_blob {
                use std::io::Write;
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                let directory = std::path::Path::new("/skyward/run/crypto/tmp/keymaster_key_blob");
                std::fs::create_dir_all(directory)?;
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(directory.join("keymaster_key_blob"))?;
                file.write_all(blob)?;
                // The TeamWin branch retains the upgraded blob in RAM;
                // it does not commit it over the source key directory.
            }
            operation.decrypt(body).map(|v| v.to_vec())
        }
    }
    impl Operation<'_> {
        pub fn decrypt(&mut self, body: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
            let mut out = Zeroizing::new(Vec::new());
            // The official operation interface caps each update at 32 KiB.
            for chunk in body.chunks(32 * 1024) {
                let mut call = Parcel::call(OPERATION);
                call.blob(chunk);
                let reply = self.handle.transact(2, &call)?;
                let mut r = reply.read();
                checked(&mut r)?;
                if let Some(mut bytes) = r.opt_blob()? {
                    out.extend_from_slice(&bytes);
                    bytes.zeroize();
                }
            }
            let mut call = Parcel::call(OPERATION);
            call.opt_blob(None);
            call.opt_blob(None);
            let reply = self.handle.transact(3, &call)?;
            let mut r = reply.read();
            checked(&mut r)?;
            self.live = false;
            if let Some(mut bytes) = r.opt_blob()? {
                out.extend_from_slice(&bytes);
                bytes.zeroize();
            }
            Ok(out)
        }
    }
    impl Drop for Operation<'_> {
        fn drop(&mut self) {
            if self.live {
                let _ = self.handle.transact(4, &Parcel::call(OPERATION));
            }
        }
    }
}
#[cfg(target_os = "android")]
pub use android::*;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_errors_keep_codes_and_redact_remote_material() {
        let reply = skyward_binder::status::Refusal::service(-62, "SECRET_KEY_DESCRIPTOR").reply();
        let error = checked(&mut Reader::new(reply.bytes()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("-62"));
        assert!(!error.contains("SECRET_KEY_DESCRIPTOR"));
    }
    #[test]
    fn sp_gcm_parameters_match_keymint_union_types_and_purpose() {
        let mut p = Parcel::default();
        gcm_parameters(&mut p, &[], &[0x42; 12], false);
        let mut r = Reader::new(p.bytes());
        assert_eq!(r.i32().unwrap(), 6);
        let mut tags = Vec::new();
        for _ in 0..6 {
            let end = r.structure().unwrap();
            let tag = r.i32().unwrap();
            assert_eq!(r.i32().unwrap(), 1);
            let arm = r.i32().unwrap();
            tags.push(tag as u32);
            if tag as u32 == 0x900003e9 {
                assert_eq!(arm, 14);
                assert_eq!(r.blob().unwrap(), [0x42; 12]);
            } else {
                let value = r.i32().unwrap();
                if tag == 0x10000002 {
                    assert_eq!((arm, value), (1, 32));
                }
                if tag == 0x20000001 {
                    assert_eq!((arm, value), (7, 1));
                }
            }
            assert_eq!(r.here(), end);
        }
        assert_eq!(
            tags,
            [
                0x10000002, 0x20000004, 0x20000006, 0x20000001, 0x900003e9, 0x300003eb
            ]
        );
        assert_eq!(r.here(), p.bytes().len());
    }
    #[test]
    fn descriptors_use_vold_blob_and_locksettings_selinux_domains() {
        for key in [
            Descriptor::vold(b"wrapped"),
            Descriptor::locksettings("synthetic_password_1234"),
        ] {
            let mut parcel = Parcel::default();
            key.write(&mut parcel);
            let got = Descriptor::read(&mut Reader::new(parcel.bytes())).unwrap();
            assert_eq!(
                (
                    got.domain,
                    got.namespace,
                    got.alias.as_deref(),
                    got.blob.as_deref()
                ),
                (
                    key.domain,
                    key.namespace,
                    key.alias.as_deref(),
                    key.blob.as_deref()
                )
            );
        }
        let key = Descriptor::vold(b"x");
        let mut p = Parcel::default();
        key.write(&mut p);
        // Native parcelable present/size, domain BLOB and namespace 100.
        assert_eq!(&p.bytes()[8..20], &[3, 0, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0]);
    }
    #[test]
    fn hardware_auth_token_retains_host_order_and_structured_timestamp() {
        let token = Token {
            challenge: 7,
            user: 9,
            authenticator: 11,
            kind: 1,
            when: 19,
            mac: vec![0x42; 32],
        };
        let mut p = Parcel::default();
        auth_token(&mut p, &token);
        let mut r = Reader::new(p.bytes());
        let end = r.structure().unwrap();
        assert_eq!(
            (
                r.i64().unwrap(),
                r.i64().unwrap(),
                r.i64().unwrap(),
                r.i32().unwrap()
            ),
            (7, 9, 11, 1)
        );
        let time = r.structure().unwrap();
        assert_eq!(r.i64().unwrap(), 19);
        assert_eq!(r.here(), time);
        assert_eq!(r.blob().unwrap(), vec![0x42; 32]);
        assert_eq!(r.here(), end);
    }
    #[test]
    fn malformed_descriptor_cannot_cross_parcelable_boundary() {
        let mut p = Parcel::default();
        Descriptor::vold(b"blob").write(&mut p);
        p.fill(4, 8); // size too short for the namespace, alias and blob
        assert!(Descriptor::read(&mut Reader::new(p.bytes())).is_err());
    }
}
