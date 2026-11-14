//! A read-only data session survives the diagnostic process, as TWRP keeps
//! its decrypted data device for the later DE/CE steps. This is lifecycle
//! alignment, not a claim that the test kernel's unmount path is repaired.
//! The RAM-only record contains identity, never keys or user credentials.
use anyhow::{Context, Result, ensure};

const HEADER: &str = "venus-readonly-data-session-v1";
#[cfg(target_os = "android")]
const RECORD: &str = "/tmp/skyward-km41-data-session.txt";
#[cfg(target_os = "android")]
const MOUNT: &str = "/mnt/skyward-data-ro";
#[cfg(target_os = "android")]
const MAPPING: &str = "skyward-km41-data-ro";

#[derive(Debug, PartialEq, Eq)]
struct Session {
    boot: String,
    source: (u32, u32),
    mapped: (u32, u32),
    mount_id: i32,
}

fn valid_boot(boot: &str) -> bool {
    boot.len() == 36
        && boot.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

impl Session {
    fn text(&self) -> String {
        format!(
            "{HEADER}\nboot={}\nsource={}:{}\nmapped={}:{}\nmount_id={}\n",
            self.boot, self.source.0, self.source.1, self.mapped.0, self.mapped.1, self.mount_id
        )
    }

    fn parse(text: &str) -> Result<Self> {
        ensure!(text.len() <= 256, "oversized data session record");
        let lines: Vec<_> = text.lines().collect();
        ensure!(
            lines.len() == 5 && lines[0] == HEADER,
            "unknown data session record"
        );
        let boot = lines[1]
            .strip_prefix("boot=")
            .context("missing boot identity")?;
        ensure!(valid_boot(boot), "invalid boot identity");
        let dev = |line: &str, prefix: &str| -> Result<(u32, u32)> {
            let (major, minor) = line
                .strip_prefix(prefix)
                .context("missing device identity")?
                .split_once(':')
                .context("invalid device identity")?;
            ensure!(
                !major.is_empty()
                    && !minor.is_empty()
                    && major
                        .bytes()
                        .chain(minor.bytes())
                        .all(|c| c.is_ascii_digit()),
                "non-numeric device identity"
            );
            let pair = (major.parse()?, minor.parse()?);
            ensure!(pair.0 > 0, "invalid device major");
            Ok(pair)
        };
        let mount_id = lines[4]
            .strip_prefix("mount_id=")
            .context("missing mount ID")?
            .parse()?;
        ensure!(mount_id > 0, "invalid mount ID");
        Ok(Self {
            boot: boot.into(),
            source: dev(lines[2], "source=")?,
            mapped: dev(lines[3], "mapped=")?,
            mount_id,
        })
    }
}

#[cfg(target_os = "android")]
mod runtime {
    use super::*;
    use skyward_block::{dm, mount};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::{
        ffi::CString,
        fs::{self, OpenOptions},
        io::Write,
        path::Path,
    };

    fn snapshot(source: &str) -> Result<Session> {
        let mapped = dm::status(MAPPING)?.context("data session mapping missing")?;
        ensure!(
            dm::readonly(MAPPING)?,
            "data mapping is no longer read-only"
        );
        let visible = mount::visible(Path::new(MOUNT))?.context("data session mount missing")?;
        ensure!(
            visible.device == mapped && visible.filesystem == "f2fs",
            "data mount no longer matches its owned mapping"
        );
        let ids: Vec<_> = mount::mount_ids()?
            .into_iter()
            .filter(|(_, p)| p == Path::new(MOUNT))
            .collect();
        ensure!(ids.len() == 1, "stacked or absent diagnostic data mount");
        let path = CString::new(MOUNT).expect("constant mount path");
        let mut info: libc::statvfs = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { libc::statvfs(path.as_ptr(), &mut info) } == 0,
            "cannot query diagnostic data mount flags"
        );
        ensure!(
            info.f_flag & libc::ST_RDONLY != 0,
            "diagnostic filesystem is not read-only"
        );
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .to_owned();
        ensure!(valid_boot(&boot), "invalid running boot identity");
        Ok(Session {
            boot,
            source: skyward_block::devnum(source)?,
            mapped,
            mount_id: ids[0].0,
        })
    }

    /// Reuse only a mount created by this protocol, in this same boot and
    /// with the same block device, mount ID and read-only flags.
    pub fn reuse(source: &str) -> Result<bool> {
        let meta = match fs::symlink_metadata(RECORD) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ensure!(
                    dm::status(MAPPING)?.is_none(),
                    "unowned data mapping exists; refusing replacement"
                );
                ensure!(
                    !mount::is_mount_point(Path::new(MOUNT))?,
                    "unowned data mount exists; refusing replacement"
                );
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        };
        ensure!(
            meta.is_file()
                && !meta.file_type().is_symlink()
                && meta.uid() == 0
                && meta.mode() & 0o077 == 0
                && meta.len() <= 256,
            "unsafe data session record"
        );
        let saved = Session::parse(&fs::read_to_string(RECORD)?)?;
        let actual = snapshot(source)?;
        ensure!(
            saved == actual,
            "stale or changed data session; no mount/mapping was altered"
        );
        Ok(true)
    }

    /// Create a RAM-only receipt after validating an owned, read-only mount.
    /// Never overwrite an existing receipt or perform teardown on failure.
    pub fn save(source: &str) -> Result<()> {
        let session = snapshot(source)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .mode(0o600)
            .open(RECORD)?;
        file.write_all(session.text().as_bytes())?;
        file.sync_all()?;
        Ok(())
    }
}
#[cfg(target_os = "android")]
pub use runtime::{reuse, save};

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Session {
        Session {
            boot: "24a9c7d1-067f-4ac0-9706-bf8a0c8d5c05".into(),
            source: (8, 34),
            mapped: (253, 5),
            mount_id: 71,
        }
    }
    #[test]
    fn session_roundtrip_contains_only_identity() {
        let s = sample();
        assert_eq!(Session::parse(&s.text()).unwrap(), s);
        assert!(!s.text().contains("key="));
    }
    #[test]
    fn stale_boot_or_replaced_mount_does_not_match() {
        let s = sample();
        let mut other = Session::parse(&s.text()).unwrap();
        other.mount_id += 1;
        assert_ne!(s, other);
        other = Session::parse(&s.text()).unwrap();
        other.boot.replace_range(..1, "3");
        assert_ne!(s, other);
        other = Session::parse(&s.text()).unwrap();
        other.mapped.1 += 1;
        assert_ne!(s, other);
    }
    #[test]
    fn duplicate_and_unknown_fields_are_rejected() {
        assert!(Session::parse(&(sample().text() + "boot=duplicate\n")).is_err());
        assert!(Session::parse(&sample().text().replace("source=", "extra=")).is_err());
    }
    #[test]
    fn malformed_and_oversized_records_are_rejected() {
        for input in [
            String::new(),
            "x".repeat(257),
            sample().text().replace("mount_id=71", "mount_id=-1"),
            sample().text().replace("source=8:34", "source=0:0"),
            sample().text().replace("source=8:34", "source=8:+34"),
            sample().text().replace("24a9c7d1", "not-uuid"),
        ] {
            assert!(Session::parse(&input).is_err());
        }
    }
}
