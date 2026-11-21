//! Binder endpoints whose wire format is not an AIDL Parcel.
use crate::{hidl, parcel::Parcel};

/// Kernel supplied sender identity and transaction metadata. Identity is never
/// read from the request body.
pub struct Call<'a> {
    pub object: u64,
    pub code: u32,
    pub flags: u32,
    pub pid: i32,
    pub uid: u32,
    pub(crate) data: &'a [u8],
    pub(crate) mapping: (usize, usize),
}

impl Call<'_> {
    pub fn bytes(&self) -> &[u8] { self.data }
    pub fn hidl(&self) -> hidl::Answer<'_> {
        hidl::Answer::bounded(self.data, self.mapping.0, self.mapping.1)
    }
    pub fn one_way(&self) -> bool { self.flags & 1 != 0 }
}

pub enum Reply {
    Hidl(hidl::Message),
    Parcel(Parcel),
    /// A transport failure, carried with TF_STATUS_CODE, not a successful
    /// method result. In particular -32 tells clients their owner is gone.
    Status(i32),
}

pub trait Service<C>: Send + Sync {
    fn call(&self, with: &C, call: Call<'_>) -> Reply;
    fn release(&self, _with: &C, _object: u64) {}
}
