use crate::{common::*, runtime::*};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}
fn cancelled() -> Result<()> {
    ensure!(!STOP.load(Ordering::Relaxed), "modem preparation cancelled");
    Ok(())
}
#[derive(Clone, Debug)]
struct Mount {
    device: PathBuf,
    target: PathBuf,
    filesystem: String,
    options: Vec<String>,
}
fn mounts() -> Result<Vec<Mount>> {
    fs::read_to_string("/proc/mounts")?
        .lines()
        .map(|line| {
            let v: Vec<_> = line.split_whitespace().collect();
            ensure!(v.len() >= 4, "malformed mount record");
            Ok(Mount {
                device: v[0].into(),
                target: v[1].into(),
                filesystem: v[2].into(),
                options: v[3].split(',').map(str::to_owned).collect(),
            })
        })
        .collect()
}
fn mount_at(path: &Path) -> Result<Option<Mount>> {
    let mut found = mounts()?.into_iter().filter(|m| m.target == path);
    let first = found.next();
    ensure!(
        found.next().is_none(),
        "stacked mount at {}",
        path.display()
    );
    Ok(first)
}
fn identity(path: &Path) -> Result<(u64, u64)> {
    let m = fs::symlink_metadata(path)?;
    ensure!(m.is_dir(), "not a real directory");
    Ok((m.dev(), m.ino()))
}
fn same(path: &Path, original: (u64, u64)) -> Result<()> {
    ensure!(
        identity(path)? == original,
        "directory identity changed: {}",
        path.display()
    );
    Ok(())
}
fn absent(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "existing path: {}",
        path.display()
    );
    Ok(())
}
fn receipt(path: &Path, boot: &str) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_file()
            && m.uid() == 0
            && m.gid() == 0
            && m.mode() & 0o7777 == 0o600
            && fs::read_to_string(path)? == boot,
        "foreign or stale receipt: {}",
        path.display()
    );
    Ok(())
}
fn tethering_source(path: &Path) -> Result<()> {
    let m = mount_at(path)?.context("tethering mount missing")?;
    ensure!(m.filesystem == "ext4", "wrong tethering filesystem");
    let device = fs::canonicalize(&m.device)?;
    let name = device
        .file_name()
        .and_then(|n| n.to_str())
        .context("loop device name")?;
    let suffix = name
        .strip_prefix("loop")
        .context("tethering is not a loop mount")?;
    ensure!(
        !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()),
        "invalid loop device"
    );
    ensure!(
        fs::read_to_string(format!("/sys/class/block/{name}/loop/backing_file"))?.trim()
            == "/skyward/runtime/tethering.img",
        "foreign tethering loop backing file"
    );
    for opt in ["ro", "nosuid", "nodev"] {
        ensure!(
            m.options.iter().any(|o| o == opt),
            "missing tethering option {opt}"
        );
    }
    Ok(())
}
struct Preparation {
    source: PathBuf,
    staging: PathBuf,
    tethering: PathBuf,
    source_dir: Option<(u64, u64)>,
    source_mount: Option<(u64, u64)>,
    stage_dir: Option<(u64, u64)>,
    tethering_dir: Option<(u64, u64)>,
    tethering_mounted: bool,
}
impl Preparation {
    fn release_source(&mut self) -> Result<()> {
        if let Some(id) = self.source_mount {
            same(&self.source, id)?;
            let mount = mount_at(&self.source)?.context("owned persist mount disappeared")?;
            ensure!(
                mount.filesystem == "ext4"
                    && fs::canonicalize(mount.device)?
                        == fs::canonicalize("/dev/block/by-name/persist")?,
                "foreign persist source mount"
            );
            cv(unsafe { libc::umount(c(&self.source)?.as_ptr()) })?;
            self.source_mount = None;
        }
        if let Some(id) = self.source_dir {
            same(&self.source, id)?;
            fs::remove_dir(&self.source)?;
            self.source_dir = None;
        }
        Ok(())
    }
    fn cleanup(&mut self) -> Result<()> {
        self.release_source()?;
        if let Some(id) = self.stage_dir {
            same(&self.staging, id)?;
            ensure!(
                !mounts()?
                    .iter()
                    .any(|m| m.target.starts_with(&self.staging)),
                "RFS staging acquired a mount; retained"
            );
            fs::remove_dir_all(&self.staging)?;
            self.stage_dir = None;
        }
        if self.tethering_mounted {
            tethering_source(&self.tethering)?;
            cv(unsafe { libc::umount(c(&self.tethering)?.as_ptr()) })?;
            self.tethering_mounted = false;
        }
        if let Some(id) = self.tethering_dir {
            same(&self.tethering, id)?;
            fs::remove_dir(&self.tethering)?;
            self.tethering_dir = None;
        }
        Ok(())
    }
}
pub fn prepare() -> Result<()> {
    integrated()?;
    ensure!(fs_type("/tmp")? == 0x01021994, "scratch must be RAM");
    let cache = Path::new("/skyward/run/hal/cache");
    ensure!(
        cache.join("vendor/rfs").is_dir(),
        "missing vendor RFS runtime"
    );
    verify(
        &cache.join("system/system/apex/com.android.tethering.capex"),
        "327c191797ac3cbfb4562df5423ae0129a68a4f02f4d91b7171a526e861b1a35",
    )?;
    verify(
        Path::new("/skyward/runtime/tethering.img"),
        "2113920f4b3c255773f69940ff74beef8d84681aef7bafbb6bd05d797da45714",
    )?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let base = Path::new(HAL);
    let published = base.join("network-persist");
    let receipt_path = base.join("apex-tethering.skyward-boot");
    let mut guard = Preparation {
        source: base.join(format!("network-persist-source.{}", std::process::id())),
        staging: base.join(format!("network-persist.new.{}", std::process::id())),
        tethering: base.join("apex-tethering"),
        source_dir: None,
        source_mount: None,
        stage_dir: None,
        tethering_dir: None,
        tethering_mounted: false,
    };
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = stop as *const () as usize;
    for sig in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
        cv(unsafe { libc::sigaction(sig, &action, std::ptr::null_mut()) })?;
    }
    let result = (|| -> Result<()> {
        if fs::symlink_metadata(&published).is_ok() {
            let m = fs::symlink_metadata(&published)?;
            ensure!(
                m.is_dir() && m.uid() == 0 && m.gid() == 2903 && m.mode() & 0o7777 == 0o750,
                "foreign RFS workspace"
            );
            receipt(&published.join(".skyward-boot"), &boot)?;
        } else {
            absent(&published)?;
            absent(&guard.staging)?;
            absent(&guard.source)?;
            fs::create_dir(&guard.staging)?;
            guard.stage_dir = Some(identity(&guard.staging)?);
            fs::create_dir(&guard.source)?;
            guard.source_dir = Some(identity(&guard.source)?);
            mount(
                Some(Path::new("/dev/block/by-name/persist")),
                &guard.source,
                Some("ext4"),
                libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                Some("noload"),
            )?;
            guard.source_mount = Some(identity(&guard.source)?);
            cancelled()?;
            let mut manifest = String::new();
            for part in ["rfs", "hlos_rfs"] {
                let path = guard.source.join(part);
                ensure!(
                    fs::symlink_metadata(&path)?.is_dir(),
                    "invalid persist calibration source"
                );
                let status = std::process::Command::new("/system/bin/cp")
                    .arg("-a")
                    .arg(&path)
                    .arg(&guard.staging)
                    .status()?;
                ensure!(status.success(), "copy persist calibration to RAM failed");
                let mut files = Vec::new();
                crate::cache::files(&path, &mut files)?;
                files.sort();
                for file in files {
                    let rel = file
                        .strip_prefix(&guard.source)?
                        .to_str()
                        .context("invalid calibration path")?;
                    ensure!(
                        !rel.contains(['\n', '\r', '\\']),
                        "invalid calibration manifest path"
                    );
                    manifest.push_str(&format!("{}  {rel}\n", hash(&file)?));
                }
                cancelled()?;
            }
            fs::write(guard.staging.join(".source.sha256"), &manifest)?;
            if !manifest.is_empty() {
                verify_manifest(&guard.staging, ".source.sha256")?;
            }
            fs::write(
                guard.staging.join(".verified"),
                "All calibration source hashes verified\n",
            )?;
            guard.release_source()?;
            cancelled()?;
            fs::write(guard.staging.join(".skyward-boot"), &boot)?;
            own(&guard.staging.join(".skyward-boot"), 0, 0, 0o600)?;
            own(&guard.staging, 0, 2903, 0o750)?;
            fs::rename(&guard.staging, &published)?;
            guard.stage_dir = None;
        }
        cancelled()?;
        if mount_at(&guard.tethering)?.is_some() {
            receipt(&receipt_path, &boot)?;
        } else {
            absent(&guard.tethering)?;
            absent(&receipt_path)?;
            fs::create_dir(&guard.tethering)?;
            guard.tethering_dir = Some(identity(&guard.tethering)?);
            checked(
                "/system/bin/mount",
                &[
                    "-t",
                    "ext4",
                    "-o",
                    "ro,noload,loop,nosuid,nodev",
                    "/skyward/runtime/tethering.img",
                    guard.tethering.to_str().unwrap(),
                ],
            )?;
            guard.tethering_mounted = true;
        }
        tethering_source(&guard.tethering)?;
        ensure!(
            guard
                .tethering
                .join("lib64/libcom.android.tethering.connectivity_native.so")
                .is_file(),
            "missing tethering platform ABI"
        );
        cancelled()?;
        if guard.tethering_mounted {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&receipt_path)?;
            file.write_all(boot.as_bytes())?;
            guard.tethering_mounted = false;
            guard.tethering_dir = None;
        }
        Ok(())
    })();
    guard
        .cleanup()
        .context("cannot safely release owned modem preparation state; retained")?;
    result?;
    println!("VENUS_MODEM_PREPARED persist_source_unmounted=true platform_abi=verified");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_rejects_replaced_directory_before_deleting() {
        let temp = tempfile::tempdir().unwrap();
        let p = temp.path().join("stage");
        fs::create_dir(&p).unwrap();
        let id = identity(&p).unwrap();
        fs::rename(&p, temp.path().join("saved")).unwrap();
        fs::create_dir(&p).unwrap();
        fs::write(p.join("foreign"), b"keep").unwrap();
        let mut guard = Preparation {
            source: temp.path().join("source"),
            staging: p.clone(),
            tethering: temp.path().join("tethering"),
            source_dir: None,
            source_mount: None,
            stage_dir: Some(id),
            tethering_dir: None,
            tethering_mounted: false,
        };
        assert!(guard.cleanup().is_err());
        assert_eq!(fs::read(p.join("foreign")).unwrap(), b"keep");
    }
    #[test]
    fn stale_or_symlink_receipt_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let p = temp.path().join("receipt");
        fs::write(&p, "other-boot\n").unwrap();
        assert!(receipt(&p, "current-boot\n").is_err());
        let link = temp.path().join("alias");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        assert!(receipt(&link, "other-boot\n").is_err());
    }
}
