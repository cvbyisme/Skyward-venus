//! Export only recovery security runtime assets, never the recovery binary,
//! init, kernel, device HALs or partition layout. The source image is read-only.
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use skyward_bootimg::{BootImage, Ramdisk};
use std::{fs, path::PathBuf};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 2,
        "usage: export_crypto_runtime REFERENCE_IMAGE NEW_OUTPUT_DIRECTORY"
    );
    let input = PathBuf::from(&args[0]);
    let output = PathBuf::from(&args[1]);
    ensure!(
        !output.exists(),
        "output already exists; never overwrite an export"
    );
    let image = fs::read(&input)?;
    let boot = BootImage::parse(&image)?;
    let source = Ramdisk::decode_bounded(&boot.ramdisk, 512 * 1024 * 1024, 32768)?;
    let required = [
        "system/bin/keystore2",
        "system/bin/servicemanager",
        "system/bin/linker64",
        "system/bin/vold_prepare_subdirs",
        "system/bin/toybox",
        "system/bin/fsck.f2fs",
        "system/bin/e2fsck",
        "system/lib64/libkm_compat.so",
        "system/lib64/libkm_compat_service.so",
        "system/etc/selinux/plat_keystore2_key_contexts",
        "system/etc/selinux/plat_service_contexts",
        "system/etc/recovery.fstab",
        "system/etc/twrp.flags",
        "system/etc/vintf/manifest.xml",
    ];
    for name in required {
        ensure!(source.find(name).is_some(), "reference lacks {name}");
    }
    let mut selected = Ramdisk::empty();
    // Android Toybox treats comment lines in a checksum list as filenames.
    // Keep provenance in a separately checksummed asset, so every manifest
    // line is accepted by the recovery's actual sha256sum implementation.
    let provenance = format!("reference-image-sha256={:x}\n", Sha256::digest(&image));
    let provenance_path = "system/etc/skyward-crypto-reference.txt";
    let mut sums = format!("{:x}  {provenance_path}\n", Sha256::digest(provenance.as_bytes()));
    selected.add(provenance_path, 0o444, provenance.into_bytes())?;
    for entry in &source.entries {
        let selected_path = entry.name.starts_with("system/lib64/")
            || required.contains(&entry.name.as_str())
            || matches!(
                entry.name.as_str(),
                "system/etc/task_profiles.json"
                    | "system/etc/cgroups.json"
                    | "system/etc/selinux/plat_file_contexts"
                    | "file_contexts"
                    | "system/etc/ramdisk/build.prop"
                    | "system/etc/init/keystore2.rc"
                    | "system/etc/vintf/manifest.xml"
            );
        if !selected_path || entry.is_dir() {
            continue;
        }
        // Keep the export self-contained. The supplied image's selected
        // runtime entries must be regular files, with no aliases out of it.
        ensure!(
            entry.mode & 0o170000 == 0o100000,
            "non-regular security runtime asset {}",
            entry.name
        );
        sums.push_str(&format!(
            "{:x}  {}\n",
            Sha256::digest(&entry.data),
            entry.name
        ));
        selected.add(&entry.name, entry.mode & 0o777, entry.data.clone())?;
    }
    fs::create_dir(&output)?;
    let root = output.join("root");
    fs::create_dir(&root)?;
    // The host export belongs to the invoking developer; root ownership
    // is retained in the cpio, without requiring chown on the workstation.
    use std::os::unix::fs::PermissionsExt;
    for entry in selected.entries.iter().filter(|e| !e.is_dir()) {
        ensure!(
            std::path::Path::new(&entry.name)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "invalid runtime path"
        );
        let path = root.join(&entry.name);
        fs::create_dir_all(path.parent().expect("runtime file has a parent"))?;
        fs::write(&path, &entry.data)?;
        fs::set_permissions(path, fs::Permissions::from_mode(entry.mode & 0o777))?;
    }
    fs::write(output.join("SOURCE.sha256"), sums)?;
    fs::write(output.join("runtime.cpio.lz4"), selected.encode()?)?;
    println!(
        "exported {} assets; reference image unchanged",
        selected.entries.len()
    );
    Ok(())
}
