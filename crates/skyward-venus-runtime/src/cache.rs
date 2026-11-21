use crate::{common::*, runtime::*};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Instant,
};
pub const ENTRIES: &[(&str, &str)] = &[
    ("system", "system/lib64"),
    ("system", "system/etc"),
    ("system", "system/bin/servicemanager"),
    ("system", "system/bin/iptables"),
    ("system", "system/bin/ip6tables"),
    ("system", "system/bin/ip"),
    ("system", "system/bin/netutils-wrapper-1.0"),
    ("system", "system/bin/ip-wrapper-1.0"),
    ("system", "system/bin/linker64"),
    ("system", "system/bin/bootstrap"),
    ("system", "system/build.prop"),
    ("system", "system/apex/com.android.tethering.capex"),
    ("system_ext", "lib64"),
    ("system_ext", "etc"),
    ("system_ext", "bin/hwservicemanager"),
    ("vendor", "lib64"),
    ("vendor", "etc"),
    ("vendor", "bin"),
    ("vendor", "firmware"),
    ("vendor", "rfs"),
    ("vendor", "build.prop"),
    ("vendor", "ueventd.rc"),
    ("odm", "etc"),
    ("product", "etc"),
    ("firmware", "."),
];
pub fn files(path: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    if m.is_file() {
        found.push(path.to_owned());
    } else if m.is_dir() {
        for entry in fs::read_dir(path)? {
            files(&entry?.path(), found)?;
        }
    }
    // Directory symlinks stay symlinks. Never follow Android's absolute aliases.
    Ok(())
}
fn source(ro: &Path, firmware: &Path, part: &str) -> PathBuf {
    if part == "firmware" {
        firmware.to_owned()
    } else {
        ro.join(part)
    }
}
fn parents(base: &Path, relative: &Path) -> Result<()> {
    let mut at = base.to_owned();
    for component in relative.components() {
        if matches!(component, std::path::Component::CurDir) {
            continue;
        }
        ensure!(
            matches!(component, std::path::Component::Normal(_)),
            "invalid cache parent"
        );
        at.push(component);
        match fs::create_dir(&at) {
            Ok(()) => fs::set_permissions(&at, fs::Permissions::from_mode(0o755))?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ensure!(
                fs::symlink_metadata(&at)?.is_dir(),
                "cache parent is not a real directory"
            ),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
/// Hash the installed source and the freshly copied RAM file independently.
/// A mismatch is a hard failure; we never publish the cache receipt. The
/// grouped checks can use multiple bounded readers, but the final manifest
/// is ordered by its original sorted path index for deterministic packaging.
/// No hash/copy verification is skipped even when all files are in RAM.
fn verify_copied_paths(
    root: &Path,
    dest: &Path,
    part: &str,
    paths: &[PathBuf],
    readers: usize,
) -> Result<String> {
    let readers = readers.clamp(1, 4).min(paths.len().max(1));
    let mut verified: Vec<Option<(String, String)>> = vec![None; paths.len()];
    std::thread::scope(|scope| -> Result<()> {
        let mut workers = Vec::new();
        for worker in 0..readers {
            workers.push(
                scope.spawn(move || -> Result<Vec<(usize, String, String)>> {
                    let mut results = Vec::new();
                    for (index, path) in paths.iter().enumerate().skip(worker).step_by(readers) {
                        let rel = path.strip_prefix(root)?;
                        let relative = rel.to_str().context("non-UTF8 runtime path")?;
                        ensure!(
                            !relative.contains(['\n', '\r', '\\'])
                                && rel
                                    .components()
                                    .all(|c| matches!(c, std::path::Component::Normal(_))),
                            "unsafe runtime manifest path"
                        );
                        let copied = dest.join(part).join(rel);
                        let src_meta = fs::symlink_metadata(path)?;
                        let dst_meta = fs::symlink_metadata(&copied)?;
                        ensure!(
                            src_meta.is_file()
                                && dst_meta.is_file()
                                && src_meta.len() == dst_meta.len()
                                && src_meta.len() <= 999_999_999,
                            "runtime copied file identity/size differs: {relative}"
                        );
                        let expected = hash(path)?;
                        ensure!(
                            hash(&copied)? == expected,
                            "SHA-256 copied cache mismatch: {part}/{relative}"
                        );
                        results.push((index, relative.to_owned(), expected));
                    }
                    Ok(results)
                }),
            );
        }
        for worker in workers {
            let results = worker
                .join()
                .map_err(|_| anyhow::anyhow!("runtime digest worker panicked"))??;
            for (index, relative, digest) in results {
                ensure!(
                    verified[index].is_none(),
                    "duplicate digest for runtime component"
                );
                verified[index] = Some((relative, digest));
            }
        }
        Ok(())
    })?;
    let mut manifest = String::new();
    for entry in verified {
        let (relative, digest) = entry.context("missing cache hash result")?;
        manifest.push_str(&format!("{digest}  {relative}\n"));
    }
    Ok(manifest)
}

fn copy_group(
    ro: &Path,
    firmware: &Path,
    dest: &Path,
    part: &str,
    group: &str,
    copier: &Path,
) -> Result<()> {
    let root = source(ro, firmware, part);
    let started = Instant::now();
    let mut paths = Vec::new();
    mark(&format!(
        "VENUS_RUNTIME_CACHE_PART_BEGIN part={part} group={group}"
    ));
    for &(p, relative) in ENTRIES {
        if p != part
            || (group == "vendor-libraries" && relative != "lib64")
            || (group == "vendor-other" && relative == "lib64")
        {
            continue;
        }
        let parent = Path::new(relative)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        parents(dest, &Path::new(part).join(parent))?;
        let status = std::process::Command::new(copier)
            .arg("-a")
            .arg(root.join(relative))
            .arg(dest.join(part).join(parent))
            .status()?;
        ensure!(status.success(), "copy cache {part}/{relative}");
        files(&root.join(relative), &mut paths)?;
    }
    let copy_ms = started.elapsed().as_millis();
    paths.sort();
    let hash_start = Instant::now();
    // This was the cold-boot critical path (~1 GiB, 1103 libraries).
    // Four bounded workers avoid serial SHA-256 reads, while the separate
    // source and destination hashes preserve the previous integrity gate.
    let readers = if group == "vendor-libraries" { 4 } else { 1 };
    let manifest = verify_copied_paths(&root, dest, part, &paths, readers)?;
    let verify_ms = hash_start.elapsed().as_millis();
    fs::write(dest.join(format!("{group}.sha256")), &manifest)?;
    // Empty vendor trees are valid; every source entry was checked earlier.
    // Destination bytes were already independently SHA-256 checked above.
    // The entire cache still becomes visible only after every group succeeds.
    fs::write(
        dest.join(format!("verification-{group}.log")),
        "All copied regular files verified\n",
    )?;
    mark(&format!(
        "VENUS_RUNTIME_CACHE_PART_READY part={part} group={group}"
    ));
    println!(
        "VENUS_RUNTIME_CACHE_TIMING part={part} group={group} files={} readers={readers} copy_ms={copy_ms} hash_verify_ms={verify_ms} total_ms={}",
        paths.len(),
        started.elapsed().as_millis()
    );
    Ok(())
}
fn copy_all(ro: &Path, firmware: &Path, dest: &Path, copier: &Path) -> Result<()> {
    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for jobs in [
            vec![("system", "system")],
            vec![("vendor", "vendor-libraries")],
            vec![("vendor", "vendor-other")],
            vec![("firmware", "firmware")],
            vec![
                ("system_ext", "system_ext"),
                ("odm", "odm"),
                ("product", "product"),
            ],
        ] {
            workers.push(scope.spawn(move || -> Result<()> {
                for (part, group) in jobs {
                    copy_group(ro, firmware, dest, part, group, copier)?;
                }
                Ok(())
            }));
        }
        let mut errors = Vec::new();
        for worker in workers {
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => errors.push(format!("{e:#}")),
                Err(_) => errors.push("cache worker panicked".into()),
            }
        }
        ensure!(
            errors.is_empty(),
            "runtime copy/verification failed; cache was not published: {}",
            errors.join("; ")
        );
        Ok(())
    })
}
pub fn prepare() -> Result<()> {
    integrated()?;
    ensure!(
        fs_type("/skyward/run")? == 0x01021994,
        "runtime must be in RAM"
    );
    ensure!(!mounted("/data")?, "cannot cache with live Data");
    let base = Path::new(HAL);
    let cache = base.join("cache");
    let staging = base.join("cache.new");
    for path in [&cache, &staging] {
        ensure!(
            fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "runtime cache exists or was interrupted"
        );
    }
    directory(&staging, 0o700)?;
    let ro = Path::new("/mnt/skyward-ro");
    let firmware = Path::new("/vendor/firmware_mnt");
    mark("VENUS_RUNTIME_CACHE_PREFLIGHT_BEGIN");
    let mut kib = 0u64;
    for &(part, rel) in ENTRIES {
        let mut paths = Vec::new();
        files(&source(ro, firmware, part).join(rel), &mut paths)?;
        for path in paths {
            let size = fs::metadata(&path)?.len();
            ensure!(size <= 999_999_999, "runtime component too large");
            kib = kib
                .checked_add(size.div_ceil(1024))
                .context("cache size overflow")?;
        }
    }
    let mem = fs::read_to_string("/proc/meminfo")?;
    let available: u64 = mem
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .and_then(|v| v.split_whitespace().next())
        .context("no MemAvailable")?
        .parse()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    cv(unsafe { libc::statvfs(c"/skyward/run".as_ptr(), stat.as_mut_ptr()) })?;
    let stat = unsafe { stat.assume_init() };
    let space = stat.f_bavail.saturating_mul(stat.f_frsize) / 1024;
    ensure!(
        kib > 0 && available >= kib + 1048576 && space >= kib + 131072,
        "Insufficient RAM: source_kib={kib} available_kib={available} tmp_free_kib={space}"
    );
    fs::write(
        staging.join("entries"),
        ENTRIES
            .iter()
            .map(|(p, r)| format!("{p} {r}\n"))
            .collect::<String>(),
    )?;
    copy_all(ro, firmware, &staging, Path::new("/system/bin/cp"))?;
    let vendor = fs::read_to_string(staging.join("vendor-libraries.sha256"))?
        + &fs::read_to_string(staging.join("vendor-other.sha256"))?;
    fs::write(staging.join("vendor.sha256"), vendor)?;
    let mut logs = String::new();
    for part in [
        "system",
        "system_ext",
        "vendor-libraries",
        "vendor-other",
        "odm",
        "product",
        "firmware",
    ] {
        logs.push_str(&fs::read_to_string(
            staging.join(format!("verification-{part}.log")),
        )?);
    }
    fs::write(staging.join("verification.log"), logs)?;
    parents(&staging, Path::new("vendor/firmware_mnt"))?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    ensure!(boot.len() == 37 && boot.ends_with('\n'), "invalid boot ID");
    fs::write(
        staging.join("ready"),
        format!("venus-runtime-cache-v1\nboot={boot}"),
    )?;
    fs::write(staging.join("size-kib"), format!("{kib}\n"))?;
    fs::rename(&staging, &cache)?;
    println!(
        "VENUS_RUNTIME_CACHE_READY size_kib={kib} verified=sha256 source_partitions_untouched=true"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copies_verify_source_bytes_modes_and_absolute_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let ro = tmp.path().join("ro");
        let fw = tmp.path().join("firmware");
        let dest = tmp.path().join("cache.new");
        fs::create_dir(&fw).unwrap();
        fs::create_dir(&dest).unwrap();
        for &(part, relative) in ENTRIES {
            let p = source(&ro, &fw, part).join(relative);
            if relative == "." {
                continue;
            }
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            if [
                "lib64",
                "etc",
                "bin",
                "firmware",
                "rfs",
                "system/lib64",
                "system/etc",
                "system/bin/bootstrap",
            ]
            .contains(&relative)
            {
                fs::create_dir_all(&p).unwrap();
                fs::write(p.join("fixture"), b"bytes").unwrap();
            } else {
                fs::write(p, b"asset").unwrap();
            }
        }
        let alias = ro.join("system/system/bin/linker64");
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink("/apex/com.android.runtime/bin/linker64", alias).unwrap();
        let library = ro.join("vendor/lib64/fixture");
        fs::set_permissions(&library, fs::Permissions::from_mode(0o640)).unwrap();
        fs::write(fw.join("firmware.bin"), b"firmware").unwrap();
        copy_all(&ro, &fw, &dest, Path::new("/bin/cp")).unwrap();
        assert_eq!(
            fs::read(dest.join("vendor/lib64/fixture")).unwrap(),
            b"bytes"
        );
        assert_eq!(
            fs::metadata(dest.join("vendor/lib64/fixture"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(
            fs::read_link(dest.join("system/system/bin/linker64")).unwrap(),
            Path::new("/apex/com.android.runtime/bin/linker64")
        );
        assert_eq!(
            fs::read(dest.join("firmware/firmware.bin")).unwrap(),
            b"firmware"
        );
        assert_eq!(fs::read(&library).unwrap(), b"bytes");
        let canonical = verify_copied_paths(
            &ro.join("vendor"),
            &dest,
            "vendor",
            std::slice::from_ref(&library),
            4,
        )
        .unwrap();
        assert!(canonical.starts_with(&hash(&library).unwrap()));
        fs::write(dest.join("vendor/lib64/fixture"), b"tampered").unwrap();
        assert!(
            verify_copied_paths(&ro.join("vendor"), &dest, "vendor", &[library], 4).is_err(),
            "tampered RAM data must fail before publishing the ready receipt"
        );
        assert!(
            !dest.join("ready").exists(),
            "copy alone cannot publish a boot receipt"
        );
    }
}
