//! Compatible key-service facade. Existing callers keep the original AIDL
//! backend; legacy devices explicitly select open_hidl().
pub use crate::keymint_aidl::Hardware;
use crate::{keymaster::Keymaster, keymint_aidl::AidlKeyMint, token::Token};
use anyhow::Result;
use skyward_binder::driver::Binder;
pub const SERVICE: &str = crate::keymint_aidl::SERVICE;
enum Backend<'a> {
    Aidl(AidlKeyMint<'a>),
    Hidl(Keymaster<'a>),
}
pub struct KeyMint<'a> {
    backend: Backend<'a>,
}
impl<'a> KeyMint<'a> {
    pub fn open(binder: &'a Binder) -> Result<Self> {
        Ok(Self {
            backend: Backend::Aidl(AidlKeyMint::open(binder)?),
        })
    }
    pub fn open_hidl(hardware: &'a Binder, memory_upgrade: bool) -> Result<Self> {
        let keymaster = Keymaster::open(hardware)?;
        Ok(Self {
            backend: Backend::Hidl(if memory_upgrade {
                keymaster.allow_memory_upgrade()
            } else {
                keymaster
            }),
        })
    }
    pub fn hardware(&self) -> Result<Hardware> {
        match &self.backend {
            Backend::Aidl(k) => k.hardware(),
            Backend::Hidl(k) => k.hardware(),
        }
    }
    pub fn ephemeral_storage_key(&self, key: &[u8]) -> Result<Vec<u8>> {
        match &self.backend {
            Backend::Aidl(k) => k.ephemeral_storage_key(key),
            Backend::Hidl(k) => k.ephemeral_storage_key(key),
        }
    }
    pub fn authorizations(&self, key: &[u8], app: &[u8]) -> Result<Vec<(i32, i64)>> {
        match &self.backend {
            Backend::Aidl(k) => k.authorizations(key, app),
            Backend::Hidl(k) => k.authorizations(key, app),
        }
    }
    pub fn gcm_decrypt(
        &self,
        key: &[u8],
        app: &[u8],
        nonce: &[u8],
        body: &[u8],
        note: Option<&Token>,
    ) -> Result<Vec<u8>> {
        match &self.backend {
            Backend::Aidl(k) => k.gcm_decrypt(key, app, nonce, body, note),
            Backend::Hidl(k) => k.gcm_decrypt(key, app, nonce, body, note),
        }
    }
    pub fn usable(&self, key: &[u8], app: &[u8]) -> Result<()> {
        match &self.backend {
            Backend::Aidl(k) => k.usable(key, app),
            Backend::Hidl(k) => k.usable(key, app),
        }
    }
}
