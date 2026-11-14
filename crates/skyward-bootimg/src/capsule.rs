//! Finite, self-contained normal-entry Venus installer capsule. The template
//! excludes its own capsule and refers only to four fixed immutable RAM files.
use crate::{BootImage, Ramdisk, avb};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub const TEMPLATE_PATH: &str = "skyward/install/template.img";
pub const MANIFEST_PATH: &str = "skyward/install/manifest.json";
pub const PARTITION_SIZE: usize = 201_326_592;
pub const MAX_TEMPLATE: usize = 96 * 1024 * 1024;
pub const MAX_MANIFEST: usize = 16 * 1024;
const MAX_RAMDISK: usize = 512 * 1024 * 1024;
const INIT: &str = "system/bin/init";
const RECOVERY: &str = "system/bin/recovery";
const CONFIG: &str = "skyward/device.toml";
const MARKER: &str = "skyward/venus-recovery.txt";
pub const NORMAL_INIT_SHA256: &str =
    "38429485d5212ec36624e8d12808c83544f3ce41fcad21f07f5f29075b1b6d9b";
pub const PAYLOAD_PATHS: [&str; 4] = [
    "skyward/runtime/runtime.img",
    "skyward/runtime/vndk30.img",
    "skyward/runtime/tethering.img",
    "skyward/MiSansVF.ttf",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub device: String,
    pub carrier: String,
    pub partition_size: u64,
    pub template_sha256: String,
    pub kernel_sha256: String,
    pub native_init_sha256: String,
    pub recovery_sha256: String,
    pub config_sha256: String,
    /// Digest of the normal image before adding either capsule entry.
    pub normal_core_sha256: String,
    pub payloads: Vec<Payload>,
}
pub struct Capsule {
    pub template: Vec<u8>,
    pub manifest: Vec<u8>,
}
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn checked_entries(rd: &Ramdisk) -> Result<()> {
    let mut seen = HashSet::new();
    for e in &rd.entries {
        ensure!(
            !e.name.starts_with('/')
                && !e.name.split('/').any(|p| p == "..")
                && !e.name.contains('\0')
                && seen.insert(&e.name),
            "unsafe or duplicate CPIO entry"
        );
        ensure!(
            e.mode & 0o170000 != 0o100000 || e.nlink == 1,
            "hardlinked regular CPIO file unsupported"
        );
    }
    Ok(())
}
fn regular<'a>(rd: &'a Ramdisk, name: &str) -> Result<&'a [u8]> {
    let e = rd
        .find(name)
        .with_context(|| format!("capsule missing {name}"))?;
    ensure!(
        e.mode & 0o170000 == 0o100000 && e.uid == 0 && e.gid == 0 && e.nlink == 1,
        "capsule payload is not a root-owned regular file: {name}"
    );
    Ok(&e.data)
}
fn normal(rd: &Ramdisk, init_hash: &str) -> Result<()> {
    checked_entries(rd)?;
    ensure!(
        sha256(regular(rd, INIT)?) == init_hash,
        "capsule must retain the exact original first-stage init"
    );
    let marker = std::str::from_utf8(regular(rd, MARKER)?)?;
    ensure!(
        marker.starts_with("venus-recovery-rw-v1\n")
            && marker
                .lines()
                .filter(|line| line.starts_with("entry="))
                .eq(["entry=original_init"]),
        "capsule template is not normal-entry Recovery"
    );
    ensure!(
        skyward_config::codename(std::str::from_utf8(regular(rd, CONFIG)?)?)? == "venus",
        "capsule config is not Venus"
    );
    ensure!(
        regular(rd, RECOVERY)?.starts_with(b"\x7fELF\x02\x01"),
        "capsule recovery is not ELF64"
    );
    ensure!(
        rd.find(TEMPLATE_PATH).is_none() && rd.find(MANIFEST_PATH).is_none(),
        "recursive capsule template refused"
    );
    Ok(())
}
fn image_header(image: &BootImage) -> Result<()> {
    ensure!(
        image.header.header_version() == 3 && !image.kernel.is_empty(),
        "Venus capsule requires boot v3 and a kernel"
    );
    ensure!(
        !String::from_utf8_lossy(&image.header.cmdline[..])
            .contains("androidboot.force_normal_boot=0"),
        "forced-recovery cmdline cannot be installed"
    );
    ensure!(
        image.ramdisk.starts_with(&[0x1f, 0x8b]),
        "Venus capsule requires the original gzip format"
    );
    Ok(())
}

pub fn create(normal_image: &BootImage) -> Result<Capsule> {
    create_for(normal_image, NORMAL_INIT_SHA256, PARTITION_SIZE)
}
fn create_for(image: &BootImage, init_hash: &str, partition_size: usize) -> Result<Capsule> {
    image_header(image)?;
    let mut rd = Ramdisk::decode_bounded(&image.ramdisk, MAX_RAMDISK, 8192)?;
    normal(&rd, init_hash)?;
    let mut manifest = Manifest {
        version: 1,
        device: "venus".into(),
        carrier: "boot".into(),
        partition_size: partition_size as u64,
        template_sha256: String::new(),
        kernel_sha256: sha256(&image.kernel),
        native_init_sha256: init_hash.into(),
        recovery_sha256: sha256(regular(&rd, RECOVERY)?),
        config_sha256: sha256(regular(&rd, CONFIG)?),
        normal_core_sha256: sha256(&image.to_bytes()?),
        payloads: Vec::new(),
    };
    for path in PAYLOAD_PATHS {
        let data = regular(&rd, path)?;
        ensure!(
            !data.is_empty() && data.len() <= 192 * 1024 * 1024,
            "invalid capsule reference size"
        );
        manifest.payloads.push(Payload {
            path: path.into(),
            size: data.len() as u64,
            sha256: sha256(data),
        });
        rd.replace(path, Vec::new())?;
    }
    let template_image = BootImage {
        header: image.header.clone(),
        kernel: image.kernel.clone(),
        ramdisk: rd.encode()?,
    };
    let template = template_image.to_bytes()?;
    ensure!(
        template.len() <= MAX_TEMPLATE,
        "capsule template exceeds limit"
    );
    manifest.template_sha256 = sha256(&template);
    let manifest = serde_json::to_vec(&manifest)?;
    ensure!(
        manifest.len() <= MAX_MANIFEST,
        "capsule manifest exceeds limit"
    );
    Ok(Capsule { template, manifest })
}
pub fn attach(rd: &mut Ramdisk, capsule: &Capsule) -> Result<()> {
    ensure!(
        rd.find(TEMPLATE_PATH).is_none() && rd.find(MANIFEST_PATH).is_none(),
        "capsule already attached"
    );
    rd.add(TEMPLATE_PATH, 0o600, capsule.template.clone())?;
    rd.add(MANIFEST_PATH, 0o600, capsule.manifest.clone())
}
pub fn manifest(bytes: &[u8]) -> Result<Manifest> {
    manifest_for(bytes, NORMAL_INIT_SHA256, PARTITION_SIZE)
}
fn manifest_for(bytes: &[u8], init_hash: &str, partition_size: usize) -> Result<Manifest> {
    ensure!(bytes.len() <= MAX_MANIFEST, "oversize capsule manifest");
    let m: Manifest = serde_json::from_slice(bytes).context("capsule manifest")?;
    ensure!(
        m.version == 1
            && m.device == "venus"
            && m.carrier == "boot"
            && m.partition_size == partition_size as u64,
        "unsupported capsule identity, version or partition size"
    );
    ensure!(
        m.native_init_sha256 == init_hash,
        "capsule permits a non-original init"
    );
    ensure!(
        m.payloads.len() == PAYLOAD_PATHS.len(),
        "wrong capsule references"
    );
    for (p, expected) in m.payloads.iter().zip(PAYLOAD_PATHS) {
        ensure!(
            p.path == expected && p.size > 0 && p.size <= 192 * 1024 * 1024,
            "unsafe capsule reference"
        );
    }
    for hash in [
        &m.template_sha256,
        &m.kernel_sha256,
        &m.native_init_sha256,
        &m.recovery_sha256,
        &m.config_sha256,
        &m.normal_core_sha256,
    ]
    .into_iter()
    .chain(m.payloads.iter().map(|p| &p.sha256))
    {
        ensure!(
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid capsule SHA256"
        );
    }
    Ok(m)
}

/// Rebuild only the normal image embedded by this exact running build. The
/// caller supplies bytes from the fixed RAM paths; physical boot is never read.
pub fn rebuild(
    template: &[u8],
    manifest: &[u8],
    running_recovery: &[u8],
    read: impl FnMut(&Payload) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    rebuild_for(
        template,
        manifest,
        running_recovery,
        read,
        NORMAL_INIT_SHA256,
        PARTITION_SIZE,
    )
}
fn rebuild_for(
    template: &[u8],
    manifest_bytes: &[u8],
    running_recovery: &[u8],
    mut read: impl FnMut(&Payload) -> Result<Vec<u8>>,
    init_hash: &str,
    partition_size: usize,
) -> Result<Vec<u8>> {
    let m = manifest_for(manifest_bytes, init_hash, partition_size)?;
    ensure!(
        template.len() <= MAX_TEMPLATE && sha256(template) == m.template_sha256,
        "corrupt capsule template"
    );
    ensure!(
        sha256(running_recovery) == m.recovery_sha256,
        "capsule does not describe this running Recovery build"
    );
    let mut image = BootImage::parse(template)?;
    image_header(&image)?;
    ensure!(
        image.to_bytes()? == template,
        "noncanonical capsule boot template"
    );
    ensure!(
        sha256(&image.kernel) == m.kernel_sha256,
        "capsule kernel changed"
    );
    let mut rd = Ramdisk::decode_bounded(&image.ramdisk, MAX_RAMDISK, 8192)?;
    normal(&rd, init_hash)?;
    ensure!(
        sha256(regular(&rd, RECOVERY)?) == m.recovery_sha256
            && sha256(regular(&rd, CONFIG)?) == m.config_sha256,
        "capsule recovery or device configuration changed"
    );
    for p in &m.payloads {
        ensure!(
            regular(&rd, &p.path)?.is_empty(),
            "capsule reference is not empty"
        );
        let data = read(p)?;
        ensure!(
            data.len() as u64 == p.size && sha256(&data) == p.sha256,
            "RAM capsule reference changed: {}",
            p.path
        );
        rd.replace(&p.path, data)?;
    }
    image.ramdisk = rd.encode()?;
    ensure!(
        sha256(&image.to_bytes()?) == m.normal_core_sha256,
        "normal Recovery core reconstruction differs"
    );
    attach(
        &mut rd,
        &Capsule {
            template: template.to_vec(),
            manifest: manifest_bytes.to_vec(),
        },
    )?;
    image.ramdisk = rd.encode()?;
    let bytes = image.to_bytes()?;
    avb::footed(&bytes, "boot", partition_size, &Sha256::digest(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use abootimg_oxide::{HeaderV3, OsVersionPatch};
    use std::io::Write;
    fn sample() -> (BootImage, String) {
        let mut rd = Ramdisk::empty();
        let init = b"original first-stage init";
        rd.add(INIT, 0o755, init.to_vec()).unwrap();
        rd.add(RECOVERY, 0o755, b"\x7fELF\x02\x01test-build".to_vec())
            .unwrap();
        rd.add(CONFIG, 0o644, b"[device]\ncodename = \"venus\"\n".to_vec())
            .unwrap();
        rd.add(
            MARKER,
            0o644,
            b"venus-recovery-rw-v1\nentry=original_init\n".to_vec(),
        )
        .unwrap();
        for (n, p) in PAYLOAD_PATHS.iter().enumerate() {
            rd.add(p, 0o600, vec![n as u8 + 1; 1024]).unwrap();
        }
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gzip.write_all(&rd.archive().unwrap()).unwrap();
        let image = BootImage {
            header: HeaderV3 {
                kernel_size: 4096,
                ramdisk_size: 0,
                osversionpatch: OsVersionPatch(0),
                cmdline: Box::new([0; 1536]),
                v4_signature_size: None,
            },
            kernel: vec![0x42; 4096],
            ramdisk: gzip.finish().unwrap(),
        };
        (image, sha256(init))
    }
    #[test]
    fn normal_image_is_reconstructed_exactly_without_recursive_template() {
        let (mut original, init) = sample();
        let size = 131072;
        let cap = create_for(&original, &init, size).unwrap();
        let mut rd = Ramdisk::decode(&original.ramdisk).unwrap();
        let exe = regular(&rd, RECOVERY).unwrap().to_vec();
        let rebuilt = rebuild_for(
            &cap.template,
            &cap.manifest,
            &exe,
            |p| Ok(regular(&rd, &p.path)?.to_vec()),
            &init,
            size,
        )
        .unwrap();
        attach(&mut rd, &cap).unwrap();
        original.ramdisk = rd.encode().unwrap();
        let core = original.to_bytes().unwrap();
        assert_eq!(
            rebuilt,
            avb::footed(&core, "boot", size, &Sha256::digest(&core)).unwrap()
        );
        let installed = Ramdisk::decode(&BootImage::parse(&rebuilt).unwrap().ramdisk).unwrap();
        let next = rebuild_for(
            regular(&installed, TEMPLATE_PATH).unwrap(),
            regular(&installed, MANIFEST_PATH).unwrap(),
            &exe,
            |p| Ok(regular(&installed, &p.path)?.to_vec()),
            &init,
            size,
        )
        .unwrap();
        assert_eq!(next, rebuilt);
        let template = Ramdisk::decode(&BootImage::parse(&cap.template).unwrap().ramdisk).unwrap();
        assert!(template.find(TEMPLATE_PATH).is_none());
    }
    #[test]
    fn corruption_identity_and_size_are_refused() {
        let (image, init) = sample();
        let size = 131072;
        let cap = create_for(&image, &init, size).unwrap();
        let rd = Ramdisk::decode(&image.ramdisk).unwrap();
        let exe = regular(&rd, RECOVERY).unwrap();
        let attempt = |template: &[u8], manifest: &[u8], exe: &[u8], corrupt: bool| {
            rebuild_for(
                template,
                manifest,
                exe,
                |p| {
                    let mut b = regular(&rd, &p.path)?.to_vec();
                    if corrupt {
                        b[0] ^= 1;
                    }
                    Ok(b)
                },
                &init,
                size,
            )
        };
        assert!(attempt(&cap.template, &cap.manifest, b"other running build", false).is_err());
        assert!(attempt(&cap.template, &cap.manifest, exe, true).is_err());
        let mut bad = cap.template.clone();
        bad[4096] ^= 1;
        assert!(attempt(&bad, &cap.manifest, exe, false).is_err());
        let mut m: Manifest = serde_json::from_slice(&cap.manifest).unwrap();
        m.payloads[0].path = "../../data".into();
        assert!(attempt(&cap.template, &serde_json::to_vec(&m).unwrap(), exe, false).is_err());
        assert!(
            rebuild_for(
                &cap.template,
                &cap.manifest,
                exe,
                |_| unreachable!(),
                &init,
                size - 1
            )
            .is_err()
        );
        assert!(Ramdisk::decode_bounded(&image.ramdisk, 100, 8192).is_err());
        assert!(Ramdisk::decode_bounded(&image.ramdisk, MAX_RAMDISK, 1).is_err());
    }
    #[test]
    fn temporary_first_stage_and_nested_capsules_are_refused() {
        let (mut image, init) = sample();
        let mut rd = Ramdisk::decode(&image.ramdisk).unwrap();
        rd.replace(INIT, b"patched init".to_vec()).unwrap();
        image.ramdisk = rd.encode().unwrap();
        assert!(create_for(&image, &init, 131072).is_err());
        rd.replace(INIT, b"original first-stage init".to_vec())
            .unwrap();
        rd.add(TEMPLATE_PATH, 0o600, b"nested".to_vec()).unwrap();
        image.ramdisk = rd.encode().unwrap();
        assert!(create_for(&image, &init, 131072).is_err());
    }
    #[test]
    fn forged_cpio_lengths_are_refused_before_allocating_them() {
        let (image, _) = sample();
        let rd = Ramdisk::decode(&image.ramdisk).unwrap();
        for offset in [54, 94] {
            let mut archive = rd.archive().unwrap();
            archive[offset..offset + 8].copy_from_slice(b"ffffffff");
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(&archive).unwrap();
            assert!(
                Ramdisk::decode_bounded(&encoder.finish().unwrap(), MAX_RAMDISK, 8192).is_err()
            );
        }
    }
}
