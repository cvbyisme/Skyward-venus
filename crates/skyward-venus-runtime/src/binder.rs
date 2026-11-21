use crate::common::*;
use anyhow::{Result, ensure};
use std::{
    fs::{self, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, MetadataExt},
    },
    path::Path,
};
#[repr(C)]
struct BinderDevice {
    name: [u8; 256],
    major: u32,
    minor: u32,
}
const BINDER_CTL_ADD: libc::c_ulong = 0xc1086201;
pub fn mount_binder(base: &Path) -> Result<()> {
    directory(base, 0o755)?;
    // The Venus kernel prepopulates three nodes, including private instances.
    if fs_type(base)? != 0x6c6f6f70 {
        mount(
            Some(Path::new("binder")),
            base,
            Some("binder"),
            libc::MS_NOSUID | libc::MS_NOEXEC,
            Some("max=3"),
        )?;
    }
    Ok(())
}
pub fn node(base: &Path, name: &str, mode: u32) -> Result<()> {
    let path = base.join(name);
    if !path.try_exists()? {
        let fd = OpenOptions::new()
            .read(true)
            .write(true)
            .open(base.join("binder-control"))?;
        let mut dev = BinderDevice {
            name: [0; 256],
            major: 0,
            minor: 0,
        };
        ensure!(name.len() < dev.name.len(), "binder name too long");
        dev.name[..name.len()].copy_from_slice(name.as_bytes());
        cv(unsafe { libc::ioctl(fd.as_raw_fd(), BINDER_CTL_ADD as _, &mut dev) })?;
        println!("created {name} ({}:{})", dev.major, dev.minor);
    }
    ensure!(
        fs::symlink_metadata(&path)?.file_type().is_char_device(),
        "not a real Binder node"
    );
    own(&path, 0, 0, mode)
}
pub fn setup() -> Result<()> {
    recovery()?;
    let base = Path::new("/dev/binderfs");
    mount_binder(base)?;
    for name in ["binder", "hwbinder", "vndbinder"] {
        node(base, name, 0o666)?;
        let p = base.join(name);
        let link = Path::new("/dev").join(name);
        match fs::symlink_metadata(&link) {
            Ok(_) => {
                let m = fs::metadata(&link)?;
                ensure!(
                    m.file_type().is_char_device() && m.rdev() == fs::metadata(&p)?.rdev(),
                    "refusing to replace existing IPC endpoint {}",
                    link.display()
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::os::unix::fs::symlink(&p, link)?
            }
            Err(e) => return Err(e.into()),
        }
    }
    println!("BINDERFS_READY; existing endpoints retained");
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn binderfs_uapi_layout() {
        assert_eq!(std::mem::size_of::<super::BinderDevice>(), 264);
    }
}
