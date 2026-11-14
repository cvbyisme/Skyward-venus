//! HIDL Keymaster 4.1 client for devices that predate AIDL KeyMint.
//! Wire layout verified against AOSP VNDK v30 generated Keymaster 4.0
//! types.h. Inherited methods use the 4.0 descriptor, not the 4.1 one.
//! No key generation/deletion, attestation changes or credential guessing.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]
use crate::token::Token;
use anyhow::{Context, Result, ensure};
use skyward_binder::hidl::{Answer, Message};
use skyward_binder::status::Fault;
#[cfg(target_os = "android")]
use zeroize::Zeroize;

pub const INTERFACE: &str = "android.hardware.keymaster@4.1::IKeymasterDevice";
const BASE: &str = "android.hardware.keymaster@4.0::IKeymasterDevice";
const PARAM_SIZE: usize = 32;
const MAX_PARAMS: usize = 1024;
const MAX_BLOB: usize = 256 * 1024;
const BLOCK_MODE: u32 = 0x2000_0004;
const PADDING: u32 = 0x2000_0006;
const MAC_LENGTH: u32 = 0x3000_03eb;
const APPLICATION_ID: u32 = 0x9000_0259;
const NONCE: u32 = 0x9000_03e9;
const GET_HARDWARE: u32 = 1;
const CHARACTERISTICS: u32 = 9;
const EXPORT: u32 = 10;
const UPGRADE: u32 = 12;
const BEGIN: u32 = 16;
const UPDATE: u32 = 17;
const FINISH: u32 = 18;
const ABORT: u32 = 19;

#[derive(Clone)]
enum Param<'a> {
    Number(u32, u64),
    Bytes(u32, &'a [u8]),
}

/// Android's km_compat prefixes hardware key blobs; software blobs must not
/// be passed to a hardware HAL. Nothing on disk is rewritten here.
fn hardware_blob(blob: &[u8]) -> Result<&[u8]> {
    ensure!(
        !blob.is_empty() && blob.len() <= MAX_BLOB,
        "invalid key blob size"
    );
    if blob.len() >= 8 && &blob[..7] == b"pKMblob" {
        match blob[7] {
            0 => {
                ensure!(blob.len() > 8, "empty prefixed hardware blob");
                return Ok(&blob[8..]);
            }
            1 => anyhow::bail!("software KeyMint blob cannot be sent to HIDL Keymaster"),
            _ => {} // Same invalid-prefix behavior as AOSP km_compat.
        }
    }
    Ok(blob)
}

fn frame(count: usize) -> Vec<u8> {
    let mut out = vec![0; 16];
    out[8..12].copy_from_slice(&(count as u32).to_le_bytes());
    out
}

fn parameters(call: &mut Message, params: &[Param<'_>]) -> Result<()> {
    ensure!(params.len() <= MAX_PARAMS, "too many HIDL parameters");
    let parent = call.buffer(frame(params.len()));
    let mut raw = vec![0; params.len() * PARAM_SIZE];
    for (i, param) in params.iter().enumerate() {
        let at = i * PARAM_SIZE;
        match param {
            Param::Number(tag, value) => {
                raw[at..at + 4].copy_from_slice(&tag.to_le_bytes());
                raw[at + 8..at + 16].copy_from_slice(&value.to_le_bytes());
            }
            Param::Bytes(tag, bytes) => {
                ensure!(bytes.len() <= MAX_BLOB, "HIDL parameter too large");
                raw[at..at + 4].copy_from_slice(&tag.to_le_bytes());
                raw[at + 24..at + 28].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
            }
        }
    }
    let vector = call.embedded_buffer(parent, 0, raw)?;
    // Generated writeEmbeddedToParcel visits every blob, including empty ones.
    for (i, param) in params.iter().enumerate() {
        let bytes = match param {
            Param::Bytes(_, bytes) => bytes.to_vec(),
            _ => Vec::new(),
        };
        call.embedded_buffer(vector, i * PARAM_SIZE + 16, bytes)?;
    }
    Ok(())
}

fn auth(call: &mut Message, token: Option<&Token>) -> Result<()> {
    let mut raw = vec![0; 56];
    if let Some(t) = token {
        ensure!(
            t.mac.len() == 32,
            "authentication token MAC must be 32 bytes"
        );
        raw[0..8].copy_from_slice(&t.challenge.to_le_bytes());
        raw[8..16].copy_from_slice(&t.user.to_le_bytes());
        raw[16..24].copy_from_slice(&t.authenticator.to_le_bytes());
        raw[24..28].copy_from_slice(&t.kind.to_le_bytes());
        raw[32..40].copy_from_slice(&t.when.to_le_bytes());
        raw[48..52].copy_from_slice(&32u32.to_le_bytes());
    }
    let parent = call.buffer(raw);
    call.embedded_buffer(parent, 40, token.map_or_else(Vec::new, |t| t.mac.clone()))?;
    Ok(())
}

fn empty_verification(call: &mut Message) -> Result<()> {
    let parent = call.buffer(vec![0; 56]);
    call.embedded_buffer(parent, 16, Vec::new())?; // parametersVerified
    call.embedded_buffer(parent, 40, Vec::new())?; // mac
    Ok(())
}

fn gcm_params<'a>(app: &'a [u8], nonce: &'a [u8]) -> Result<Vec<Param<'a>>> {
    ensure!(nonce.len() == 12, "AES-GCM nonce must be 12 bytes");
    let mut p = vec![
        Param::Number(BLOCK_MODE, 32),
        Param::Number(PADDING, 1),
        Param::Number(MAC_LENGTH, 128),
    ];
    if !app.is_empty() {
        p.push(Param::Bytes(APPLICATION_ID, app));
    }
    p.push(Param::Bytes(NONCE, nonce));
    // Purpose is a separate argument in HIDL begin, not an operation tag.
    Ok(p)
}

fn code(read: &mut Answer<'_>) -> Result<()> {
    read.status()?;
    let code = read.i32()?;
    if code == 0 {
        Ok(())
    } else {
        Err(Fault::Service {
            code,
            said: "HIDL Keymaster".into(),
        }
        .into())
    }
}
fn count(raw: &[u8], at: usize) -> Result<usize> {
    let field = raw
        .get(at + 8..at + 12)
        .context("truncated HIDL vector frame")?;
    Ok(u32::from_le_bytes(field.try_into()?) as usize)
}
fn vector_data<'a>(read: &mut Answer<'a>, count: usize, width: usize) -> Result<&'a [u8]> {
    let size = count
        .checked_mul(width)
        .context("HIDL vector size overflow")?;
    ensure!(size <= MAX_BLOB, "HIDL vector exceeds bound");
    let data = read.pointed()?;
    ensure!(data.len() == size, "HIDL vector length mismatch");
    Ok(data)
}
fn bytes(read: &mut Answer<'_>) -> Result<Vec<u8>> {
    let header = read.pointed()?;
    ensure!(header.len() == 16, "invalid HIDL byte-vector frame");
    Ok(vector_data(read, count(header, 0)?, 1)?.to_vec())
}
fn text(read: &mut Answer<'_>) -> Result<String> {
    let header = read.pointed()?;
    ensure!(header.len() == 16, "invalid HIDL string frame");
    let n = count(header, 0)?;
    ensure!(n < 4096, "hardware name too long");
    let data = vector_data(read, n + 1, 1)?;
    ensure!(data[n] == 0, "unterminated HIDL string");
    Ok(std::str::from_utf8(&data[..n])?.to_owned())
}
fn param_values(read: &mut Answer<'_>, n: usize) -> Result<Vec<(i32, i64)>> {
    ensure!(n <= MAX_PARAMS, "too many returned parameters");
    let raw = vector_data(read, n, PARAM_SIZE)?;
    let mut out = Vec::with_capacity(n);
    for p in raw.as_chunks::<PARAM_SIZE>().0 {
        let tag = u32::from_le_bytes(p[..4].try_into()?);
        let blob_len = count(p, 16)?;
        vector_data(read, blob_len, 1)?; // consume validated embedded blob
        let value = match tag >> 28 {
            5 | 6 | 10 => u64::from_le_bytes(p[8..16].try_into()?) as i64,
            8 | 9 => blob_len as i64,
            7 => i64::from(p[8] != 0),
            _ => u32::from_le_bytes(p[8..12].try_into()?) as i64,
        };
        out.push((tag as i32, value));
    }
    Ok(out)
}
fn parameter_vector(read: &mut Answer<'_>) -> Result<Vec<(i32, i64)>> {
    let header = read.pointed()?;
    ensure!(header.len() == 16, "invalid parameter-vector frame");
    param_values(read, count(header, 0)?)
}

#[cfg(target_os = "android")]
mod runtime {
    use super::*;
    use crate::keymint::Hardware;
    use skyward_binder::driver::{Binder, Handle};
    use skyward_binder::{hidl, status};

    pub struct Keymaster<'a> {
        device: Handle<'a>,
        memory_upgrade: bool,
    }
    impl<'a> Keymaster<'a> {
        pub fn open(binder: &'a Binder) -> Result<Self> {
            let device = hidl::find(binder, INTERFACE, "default")?
                .context("HIDL Keymaster 4.1/default is not registered")?;
            ensure!(
                hidl::describes(&device)? == INTERFACE,
                "unexpected Keymaster interface"
            );
            Ok(Self {
                device,
                memory_upgrade: false,
            })
        }
        /// Explicit opt-in; upgraded opaque blobs remain in RAM only.
        pub fn allow_memory_upgrade(mut self) -> Self {
            self.memory_upgrade = true;
            self
        }
        pub fn hardware(&self) -> Result<Hardware> {
            let reply = self.device.transact(GET_HARDWARE, &Message::call(BASE))?;
            let mut r = reply.hidl();
            r.status()?;
            Ok(Hardware {
                version: 41,
                security_level: r.i32()?,
                name: text(&mut r)?,
                author: text(&mut r)?,
            })
        }
        pub fn authorizations(&self, blob: &[u8], app: &[u8]) -> Result<Vec<(i32, i64)>> {
            let mut m = Message::call(BASE);
            m.bytes(hardware_blob(blob)?);
            m.bytes(app);
            m.bytes(&[]);
            let reply = self.device.transact(CHARACTERISTICS, &m)?;
            let mut r = reply.hidl();
            code(&mut r)?;
            let raw = r.pointed()?;
            ensure!(raw.len() == 32, "invalid KeyCharacteristics size");
            let mut out = param_values(&mut r, count(raw, 0)?)?;
            out.extend(param_values(&mut r, count(raw, 16)?)?);
            Ok(out)
        }
        fn export(&self, blob: &[u8]) -> Result<Vec<u8>> {
            let mut m = Message::call(BASE);
            // As AOSP keystore2/km_compat: export RAW is wrapped ephemeral
            // storage-key conversion, NOT exporting a plaintext AES key.
            m.u32(3);
            m.bytes(blob);
            m.bytes(&[]);
            m.bytes(&[]);
            let reply = self.device.transact(EXPORT, &m)?;
            let mut r = reply.hidl();
            code(&mut r)?;
            bytes(&mut r)
        }
        fn upgrade(&self, blob: &[u8], app: &[u8]) -> Result<Vec<u8>> {
            ensure!(
                self.memory_upgrade,
                "key requires upgrade; RAM-only upgrade not explicitly enabled"
            );
            let mut m = Message::call(BASE);
            m.bytes(blob);
            let params = if app.is_empty() {
                Vec::new()
            } else {
                vec![Param::Bytes(APPLICATION_ID, app)]
            };
            parameters(&mut m, &params)?;
            let reply = self.device.transact(UPGRADE, &m)?;
            let mut r = reply.hidl();
            code(&mut r)?;
            bytes(&mut r)
        }
        pub fn ephemeral_storage_key(&self, blob: &[u8]) -> Result<Vec<u8>> {
            let blob = hardware_blob(blob)?;
            match self.export(blob) {
                Err(e) if status::refused_with(&e) == Some(-62) && self.memory_upgrade => {
                    self.export(&self.upgrade(blob, &[])?)
                }
                result => result,
            }
        }
        fn begin(
            &self,
            blob: &[u8],
            app: &[u8],
            nonce: &[u8],
            note: Option<&Token>,
        ) -> Result<u64> {
            let mut m = Message::call(BASE);
            m.u32(1);
            m.bytes(blob);
            parameters(&mut m, &gcm_params(app, nonce)?)?;
            auth(&mut m, note)?;
            let reply = self.device.transact(BEGIN, &m)?;
            let mut r = reply.hidl();
            code(&mut r).context("Keymaster begin")?;
            parameter_vector(&mut r)?;
            let handle = r.u64()?;
            ensure!(handle != 0, "zero Keymaster operation handle");
            Ok(handle)
        }
        fn abort(&self, handle: u64) -> Result<()> {
            let mut m = Message::call(BASE);
            m.u64(handle);
            let reply = self.device.transact(ABORT, &m)?;
            code(&mut reply.hidl())
        }
        pub fn usable(&self, blob: &[u8], app: &[u8]) -> Result<()> {
            let h = self.begin(hardware_blob(blob)?, app, &[0; 12], None)?;
            self.abort(h)
        }
        fn update(&self, h: u64, input: &[u8], note: Option<&Token>) -> Result<(usize, Vec<u8>)> {
            let mut m = Message::call(BASE);
            m.u64(h);
            parameters(&mut m, &[])?;
            m.bytes(input);
            auth(&mut m, note)?;
            empty_verification(&mut m)?;
            let reply = self.device.transact(UPDATE, &m)?;
            let mut r = reply.hidl();
            code(&mut r)?;
            let consumed = r.i32()?;
            ensure!(
                consumed >= 0 && consumed as usize <= input.len(),
                "invalid Keymaster input-consumed count"
            );
            parameter_vector(&mut r)?;
            Ok((consumed as usize, bytes(&mut r)?))
        }
        fn finish(&self, h: u64, input: &[u8], note: Option<&Token>) -> Result<Vec<u8>> {
            let mut m = Message::call(BASE);
            m.u64(h);
            parameters(&mut m, &[])?;
            m.bytes(input);
            m.bytes(&[]);
            auth(&mut m, note)?;
            empty_verification(&mut m)?;
            let reply = self.device.transact(FINISH, &m)?;
            let mut r = reply.hidl();
            code(&mut r).context("Keymaster finish")?;
            parameter_vector(&mut r)?;
            bytes(&mut r)
        }
        pub fn gcm_decrypt(
            &self,
            blob: &[u8],
            app: &[u8],
            nonce: &[u8],
            body: &[u8],
            note: Option<&Token>,
        ) -> Result<Vec<u8>> {
            ensure!(
                (16..=MAX_BLOB).contains(&body.len()),
                "invalid AES-GCM ciphertext length"
            );
            let blob = hardware_blob(blob)?;
            let h = match self.begin(blob, app, nonce, note) {
                Err(e) if status::refused_with(&e) == Some(-62) && self.memory_upgrade => {
                    self.begin(&self.upgrade(blob, app)?, app, nonce, note)?
                }
                result => result?,
            };
            let mut guard = Operation {
                service: self,
                handle: h,
                done: false,
            };
            let mut out = Vec::new();
            let result = (|| -> Result<()> {
                let mut rest = body;
                while rest.len() > 16384 {
                    let (used, bytes) = self.update(h, &rest[..16384], note)?;
                    ensure!(used > 0, "Keymaster update made no progress");
                    out.extend_from_slice(&bytes);
                    rest = &rest[used..];
                    ensure!(out.len() <= body.len(), "unexpected plaintext length");
                }
                // Never release update output before GCM authentication.
                out.extend(self.finish(h, rest, note)?);
                guard.done = true;
                ensure!(
                    out.len() == body.len() - 16,
                    "unexpected authenticated plaintext size"
                );
                Ok(())
            })();
            if let Err(e) = result {
                out.zeroize();
                return Err(e);
            }
            Ok(out)
        }
    }
    struct Operation<'s, 'a> {
        service: &'s Keymaster<'a>,
        handle: u64,
        done: bool,
    }
    impl Drop for Operation<'_, '_> {
        fn drop(&mut self) {
            if !self.done {
                let _ = self.service.abort(self.handle);
            }
        }
    }
}
#[cfg(target_os = "android")]
pub use runtime::Keymaster;

#[cfg(test)]
mod tests {
    use super::*;
    use skyward_binder::parcel::Payload;
    fn buffers(m: &Message) -> Vec<(u64, u64, Vec<u8>)> {
        m.addresses()
            .iter()
            .map(|offset| {
                let p = &m.values()[*offset as usize..*offset as usize + 40];
                let addr = u64::from_le_bytes(p[8..16].try_into().unwrap());
                let n = u64::from_le_bytes(p[16..24].try_into().unwrap()) as usize;
                let bytes = if n == 0 {
                    Vec::new()
                } else {
                    unsafe { std::slice::from_raw_parts(addr as *const u8, n) }.to_vec()
                };
                (
                    u64::from_le_bytes(p[24..32].try_into().unwrap()),
                    u64::from_le_bytes(p[32..40].try_into().unwrap()),
                    bytes,
                )
            })
            .collect()
    }
    #[test]
    fn hardware_prefix_is_stripped_but_software_is_refused() {
        assert_eq!(hardware_blob(b"pKMblob\0payload").unwrap(), b"payload");
        assert_eq!(hardware_blob(b"legacy").unwrap(), b"legacy");
        assert!(hardware_blob(b"pKMblob\x01payload").is_err());
        assert!(hardware_blob(b"pKMblob\0").is_err());
        assert!(hardware_blob(b"").is_err());
    }
    #[test]
    fn inherited_methods_have_aosp_40_ordinals() {
        assert_eq!(
            [
                GET_HARDWARE,
                CHARACTERISTICS,
                EXPORT,
                UPGRADE,
                BEGIN,
                UPDATE,
                FINISH,
                ABORT
            ],
            [1, 9, 10, 12, 16, 17, 18, 19]
        );
        assert!(BASE.contains("@4.0"));
        assert!(INTERFACE.contains("@4.1"));
    }
    #[test]
    fn params_use_32_byte_structs_and_correct_blob_fixups() {
        let mut m = Message::call(BASE);
        parameters(
            &mut m,
            &[
                Param::Number(BLOCK_MODE, 32),
                Param::Bytes(NONCE, b"abcdefghijkl"),
            ],
        )
        .unwrap();
        let b = buffers(&m);
        assert_eq!(b.len(), 4);
        assert_eq!(count(&b[0].2, 0).unwrap(), 2);
        assert_eq!(b[1].2.len(), 64);
        assert_eq!(b[1].2[8], 32);
        assert_eq!((b[2].0, b[2].1), (1, 16));
        assert_eq!((b[3].0, b[3].1), (1, 48));
        assert_eq!(b[3].2, b"abcdefghijkl");
    }
    #[test]
    fn empty_binding_is_omitted_not_encoded_as_empty() {
        let a = gcm_params(&[], b"abcdefghijkl").unwrap();
        assert_eq!(a.len(), 4);
        assert!(
            !a.iter()
                .any(|p| matches!(p, Param::Bytes(APPLICATION_ID, _)))
        );
        assert!(
            !a.iter().any(|p| matches!(p, Param::Number(0x20000001, _))),
            "legacy purpose is a separate argument"
        );
        assert_eq!(gcm_params(b"id", b"abcdefghijkl").unwrap().len(), 5);
        assert!(gcm_params(&[], b"bad").is_err());
    }
    #[test]
    fn auth_token_native_layout_and_mac_are_preserved() {
        let token = Token {
            challenge: 11,
            user: 22,
            authenticator: 33,
            kind: 1,
            when: 44,
            mac: vec![5; 32],
        };
        let mut m = Message::call(BASE);
        auth(&mut m, Some(&token)).unwrap();
        let b = buffers(&m);
        assert_eq!(b[0].2.len(), 56);
        assert_eq!(b[0].2[0], 11);
        assert_eq!(b[0].2[8], 22);
        assert_eq!(b[0].2[32], 44);
        assert_eq!(count(&b[0].2, 40).unwrap(), 32);
        assert_eq!(b[1].1, 40);
        assert_eq!(b[1].2, vec![5; 32]);
    }
    #[test]
    fn empty_tokens_still_have_embedded_buffers() {
        let mut m = Message::call(BASE);
        auth(&mut m, None).unwrap();
        empty_verification(&mut m).unwrap();
        let b = buffers(&m);
        assert_eq!(b.len(), 5);
        assert_eq!(b[0].2.len(), 56);
        assert_eq!(b[2].2.len(), 56);
        assert_eq!((b[3].0, b[3].1), (2, 16));
        assert_eq!((b[4].0, b[4].1), (2, 40));
    }
    #[test]
    fn service_error_is_preserved_without_dumping_payload() {
        let mut raw = 0i32.to_le_bytes().to_vec();
        raw.extend_from_slice(&(-62i32).to_le_bytes());
        let err = code(&mut Answer::new(&raw)).unwrap_err();
        assert_eq!(skyward_binder::status::refused_with(&err), Some(-62));
    }
    #[test]
    fn malformed_frames_and_bad_token_lengths_are_refused() {
        assert!(count(&[0; 8], 0).is_err());
        let t = Token {
            challenge: 0,
            user: 0,
            authenticator: 0,
            kind: 0,
            when: 0,
            mac: vec![0; 1],
        };
        assert!(auth(&mut Message::call(BASE), Some(&t)).is_err());
        assert!(bytes(&mut Answer::new(&[])).is_err());
    }
}
