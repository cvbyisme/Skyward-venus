//! Common HIDL IBase transactions, using the public Android 11 wire contract.
use crate::{hidl::{Answer, Message}, raw::Reply};
use anyhow::Result;

pub const BASE: &str = "android.hidl.base@1.0::IBase";
pub const BASE_HASH: &str = "ec7fd79ed02dfa85bc499426adae3ebe23ef0524f3cd6957139324b83b18ca4c";

pub struct Interface {
    pub name: &'static str,
    /// Frozen hash from AOSP current.txt, not the hash of this implementation.
    pub hash: &'static str,
}

pub const BASE_INTERFACE: Interface = Interface { name: BASE, hash: BASE_HASH };

pub fn base(code: u32, one_way: bool, read: &mut Answer<'_>, chain: &[Interface]) -> Result<Reply> {
    if read.interface()? != BASE { return Ok(Reply::Status(-74)); }
    if one_way != matches!(code, 256_462_420 | 257_120_595) {
        return Ok(Reply::Status(-22));
    }
    let mut reply = Message::ok();
    match code {
        256_067_662 => { // interfaceChain
            read.finish()?;
            reply.strings(&chain.iter().map(|i| i.name).collect::<Vec<_>>())?;
        }
        256_131_655 => { // default IBase::debug is empty; driver closes input FDs.
        }
        256_136_003 => { read.finish()?; reply.text(chain[0].name); }
        256_398_152 => {
            read.finish()?;
            let hashes = chain.iter().map(|i| {
                anyhow::ensure!(i.hash.len() == 64, "invalid interface hash");
                let mut hash = [0; 32];
                for (n, byte) in hash.iter_mut().enumerate() {
                    *byte = u8::from_str_radix(&i.hash[n * 2..n * 2 + 2], 16)?;
                }
                Ok(hash)
            }).collect::<Result<Vec<_>>>()?;
            reply.hashes(&hashes)?;
        }
        256_462_420 | 256_921_159 | 257_120_595 => { read.finish()?; }
        257_049_926 => {
            read.finish()?;
            let mut info = vec![0; 24];
            info[..4].copy_from_slice(&std::process::id().to_le_bytes());
            info[16..20].copy_from_slice(&1u32.to_le_bytes()); // IS_64BIT
            reply.buffer(info);
        }
        _ => return Ok(Reply::Status(-74)),
    }
    Ok(Reply::Hidl(reply))
}

#[cfg(target_os = "android")]
pub fn register(binder: &crate::driver::Binder, name: &str, object: u64) -> Result<()> {
    let mut request = Message::call("android.hidl.manager@1.0::IServiceManager");
    request.text(name);
    request.local(object);
    let reply = binder.directory()?.transact(2, &request)?;
    let mut read = reply.hidl();
    read.status()?;
    anyhow::ensure!(read.bool()?, "hardware service manager refused registration");
    read.finish()
}
