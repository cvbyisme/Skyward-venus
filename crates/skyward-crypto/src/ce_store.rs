//! Read-only CE preflight for one existing owner protector. Originals are
//! never opened writable. SQLite and any committed WAL are copied to a
//! private scratch directory so SQLite can manage its own SHM there.
//! No credential is tried and no hardware service is called by this module.
use crate::user::{self, Lock};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zeroize::{Zeroize, Zeroizing};

const DB_LIMIT: u64 = 64 * 1024 * 1024;
const BLOB_LIMIT: u64 = 256 * 1024;

pub struct Observed {
    root: PathBuf,
    files: Vec<(PathBuf, Option<[u8; 32]>)>,
}
impl Observed {
    pub fn new(root: &Path) -> Result<Self> {
        let root = root.canonicalize().context("CE root is unavailable")?;
        Ok(Self {
            root,
            files: Vec::new(),
        })
    }
    pub fn read(
        &mut self,
        relative: &str,
        max: u64,
        optional: bool,
    ) -> Result<Option<Zeroizing<Vec<u8>>>> {
        ensure!(
            !relative.is_empty()
                && Path::new(relative)
                    .components()
                    .all(|p| matches!(p, std::path::Component::Normal(_))),
            "invalid relative CE path"
        );
        let path = self.root.join(relative);
        match fs::symlink_metadata(&path) {
            Err(e) if optional && e.kind() == std::io::ErrorKind::NotFound => {
                self.files.push((path, None));
                return Ok(None);
            }
            Err(_) => anyhow::bail!("required CE state file is unavailable"),
            Ok(m) => ensure!(
                m.is_file() && !m.file_type().is_symlink(),
                "CE state file is not a regular file"
            ),
        }
        ensure!(
            path.canonicalize()?.starts_with(&self.root),
            "CE state file escapes data root"
        );
        let bytes = bounded(&path, max)?;
        self.files.push((path, Some(Sha256::digest(&bytes).into())));
        Ok(Some(bytes))
    }
    pub fn required(&mut self, relative: &str, max: u64) -> Result<Zeroizing<Vec<u8>>> {
        self.read(relative, max, false)?.context("missing CE state")
    }
    pub fn unchanged(&self) -> Result<()> {
        for (path, expected) in &self.files {
            let now = match fs::symlink_metadata(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
                Ok(m) => {
                    ensure!(
                        m.is_file() && !m.file_type().is_symlink(),
                        "CE source file type changed"
                    );
                    Some(Sha256::digest(&bounded(path, DB_LIMIT)?).into())
                }
            };
            ensure!(
                &now == expected,
                "CE source files changed; no source file was written by this command"
            );
        }
        Ok(())
    }
}

fn bounded(path: &Path, max: u64) -> Result<Zeroizing<Vec<u8>>> {
    let before = fs::symlink_metadata(path)?;
    ensure!(
        before.is_file() && !before.file_type().is_symlink() && before.len() <= max,
        "invalid or oversized CE state file"
    );
    let file = fs::File::open(path)?;
    let opened = file.metadata()?;
    ensure!(
        opened.ino() == before.ino() && opened.dev() == before.dev(),
        "CE source changed during open"
    );
    let mut out = Zeroizing::new(Vec::new());
    file.take(max + 1).read_to_end(&mut out)?;
    ensure!(out.len() as u64 <= max, "CE state file exceeds bound");
    Ok(out)
}

struct Database {
    connection: Connection,
    _scratch: tempfile::TempDir,
}
impl Database {
    fn snapshot(source: &mut Observed, relative: &str, scratch: &Path) -> Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix(".skyward-ce-db-")
            .tempdir_in(scratch)?;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))?;
        for suffix in ["", "-wal"] {
            if let Some(bytes) =
                source.read(&format!("{relative}{suffix}"), DB_LIMIT, !suffix.is_empty())?
            {
                let path = dir.path().join(format!("snapshot.db{suffix}"));
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)?;
                file.write_all(&bytes)?;
            }
        }
        // Do not use immutable=1: it can hide committed WAL transactions.
        // The disposable copy may gain a SHM file; original DB/WAL never do.
        let db = Connection::open_with_flags(
            dir.path().join("snapshot.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        db.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;")?;
        Ok(Self {
            connection: db,
            _scratch: dir,
        })
    }
}

/// Sensitive fields intentionally have no Debug implementation.
pub struct Profile {
    pub lock: Option<Lock>,
    pub blob: Zeroizing<Vec<u8>>,
    pub secdiscardable: Zeroizing<Vec<u8>>,
    pub hardware_key: Zeroizing<Vec<u8>>,
    cached_sids: Vec<u64>,
    pub ce_directory: PathBuf,
    source: Observed,
}
impl Drop for Profile {
    fn drop(&mut self) {
        if let Some(lock) = &mut self.lock {
            lock.salt.zeroize();
            lock.enrolled.zeroize();
        }
    }
}
impl Profile {
    pub fn load(root: &Path, scratch: &Path) -> Result<Self> {
        let mut source = Observed::new(root)?;
        let id = {
            let db = Database::snapshot(&mut source, "system/locksettings.db", scratch)?;
            let mut stmt = db
                .connection
                .prepare("SELECT value FROM locksettings WHERE name='sp-handle' AND user=0")?;
            let ids = stmt
                .query_map([], |r| r.get::<_, Option<String>>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ensure!(
                ids.len() == 1,
                "owner protector is missing or ambiguous; no alternate protector will be tried"
            );
            let id = ids[0]
                .as_ref()
                .context("owner protector is not recorded")?
                .parse::<i64>()
                .context("invalid owner protector record")?;
            ensure!(id != 0, "owner protector is not recorded");
            user::protector_name(id)
        };
        let prefix = format!("system_de/0/spblob/{id}");
        let blob = source.required(&format!("{prefix}.spblob"), BLOB_LIMIT)?;
        let payload = user::content(&blob)?;
        ensure!(payload.len() >= 28, "synthetic-password blob is truncated");
        ensure!(
            source
                .read(&format!("{prefix}.weaver"), 64, true)?
                .is_none(),
            "this CE entry supports Gatekeeper protectors only; Weaver was not contacted"
        );
        let secdiscardable = source.required(&format!("{prefix}.secdis"), 16384)?;
        ensure!(
            secdiscardable.len() == 16384,
            "unsupported secdiscardable size"
        );
        let lock = source
            .read(&format!("{prefix}.pwd"), 65536, true)?
            .map(|raw| Lock::of(&raw))
            .transpose()?;
        if let Some(lock) = &lock {
            ensure!(
                matches!(lock.kind, Lock::PIN | Lock::PASSWORD | Lock::PATTERN),
                "unsupported stored credential type"
            );
            ensure!(
                lock.salt.len() == 16 && lock.enrolled.len() == 58,
                "unsupported Gatekeeper password record"
            );
            ensure!(
                lock.enrolled[0] <= 2,
                "unsupported Gatekeeper handle format"
            );
            // Validate costs now, before a local prompt or hardware request.
            validate_cost(lock.cost)?;
        }
        let alias = format!("synthetic_password_{}", id.trim_start_matches('0'));
        let (hardware_key, cached_sids) = {
            let db = Database::snapshot(&mut source, "misc/keystore/persistent.sqlite", scratch)?;
            select_key(&db.connection, &alias)?
        };
        let ce = "misc/vold/user_keys/ce/0/current";
        ensure!(
            std::str::from_utf8(&source.required(&format!("{ce}/version"), 16)?)?.trim() == "1",
            "unsupported CE wrapped-key version"
        );
        ensure!(
            source
                .read(&format!("{ce}/keymaster_key_blob"), BLOB_LIMIT, true)?
                .is_none(),
            "CE wrapper is hardware-bound; this software-wrapper path was not attempted"
        );
        ensure!(
            source
                .read(&format!("{ce}/stretching"), 128, true)?
                .is_none(),
            "legacy CE stretching is not supported by this entry"
        );
        ensure!(
            source
                .required(&format!("{ce}/encrypted_key"), BLOB_LIMIT)?
                .len()
                > 28,
            "CE wrapped key is truncated"
        );
        source.read(&format!("{ce}/secdiscardable"), 16384, true)?;
        source.unchanged()?;
        Ok(Self {
            lock,
            blob,
            secdiscardable,
            hardware_key,
            cached_sids,
            ce_directory: root.join(ce),
            source,
        })
    }
    pub fn kind(&self) -> &'static str {
        match self.lock.as_ref().map(|l| l.kind) {
            Some(Lock::PIN) => "pin",
            Some(Lock::PASSWORD) => "password",
            Some(Lock::PATTERN) => "pattern",
            None => "none",
            _ => "unsupported",
        }
    }
    pub fn unchanged(&self) -> Result<()> {
        self.source.unchanged()
    }
    pub fn cached_sid_matches(&self, sid: u64) -> bool {
        sid != 0 && self.cached_sids.contains(&sid)
    }
}

fn validate_cost(cost: [u8; 3]) -> Result<()> {
    let [n, r, p] = cost;
    ensure!(
        (1..=20).contains(&n) && r <= 10 && p <= 10,
        "unsupported credential stretching cost"
    );
    ensure!(
        128 * (1u64 << n) * (1u64 << r) <= 64 * 1024 * 1024
            && (1u64 << n) * (1u64 << r) * (1u64 << p) <= 16 * 1024 * 1024,
        "credential stretching exceeds memory/work bound"
    );
    Ok(())
}

fn select_key(db: &Connection, alias: &str) -> Result<(Zeroizing<Vec<u8>>, Vec<u64>)> {
    // Locksettings namespace 103, SELINUX domain 2, live CLIENT key.
    // Never borrow an alias from an application's or deleted key's scope.
    let mut stmt = db.prepare("SELECT id FROM keyentry WHERE alias=?1 AND domain=2 AND namespace=103 AND state=1 AND key_type=0")?;
    let ids = stmt
        .query_map([alias], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        ids.len() == 1,
        "current locksettings protector key is missing or ambiguous"
    );
    let blob: Vec<u8> = db.query_row("SELECT blob FROM blobentry WHERE keyentryid=?1 AND subcomponent_type=0 ORDER BY id DESC LIMIT 1", [ids[0]], |r|r.get(0)).context("protector key blob is unavailable")?;
    ensure!(
        !blob.is_empty() && blob.len() as u64 <= BLOB_LIMIT,
        "invalid protector key blob length"
    );
    let mut query =
        db.prepare("SELECT data FROM keyparameter WHERE keyentryid=?1 AND (tag=?2 OR tag=?3)")?;
    let sids = query
        .query_map(
            rusqlite::params![ids[0], 0xa00001f6u32 as i32 as i64, 0xa00001f6u32 as i64],
            |r| r.get::<_, i64>(0),
        )?
        .map(|r| r.map(|v| v as u64))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((Zeroizing::new(blob), sids))
}

/// Credential syntax only; this function never calls Gatekeeper.
pub fn validate_input(kind: &str, input: &[u8]) -> Result<()> {
    ensure!(input.len() <= 4096, "credential input exceeds bound");
    match kind {
        "pin" => ensure!(
            (4..=64).contains(&input.len()) && input.iter().all(u8::is_ascii_digit),
            "PIN must contain only its digits"
        ),
        "password" => ensure!(
            !input.is_empty() && std::str::from_utf8(input).is_ok() && !input.contains(&0),
            "password must be nonempty UTF-8 without NUL"
        ),
        // Pattern transport is not silently guessed. Enable it only with a
        // verified UI encoding, not by treating it as a PIN.
        "pattern" => anyhow::bail!("pattern input is not yet enabled; no credential submitted"),
        "none" => ensure!(
            input.is_empty(),
            "this protector does not accept a typed credential"
        ),
        _ => anyhow::bail!("unsupported credential type"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sqlite_snapshot_includes_committed_wal_without_writing_originals() {
        let root = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let db = Connection::open(root.path().join("test.db")).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE test(v INTEGER); INSERT INTO test VALUES(17);").unwrap();
        let before = fs::read(root.path().join("test.db")).unwrap();
        let wal = fs::read(root.path().join("test.db-wal")).unwrap();
        let mut observed = Observed::new(root.path()).unwrap();
        let snap = Database::snapshot(&mut observed, "test.db", tmp.path()).unwrap();
        assert_eq!(
            snap.connection
                .query_row("SELECT v FROM test", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            17
        );
        observed.unchanged().unwrap();
        drop(snap);
        assert_eq!(fs::read(root.path().join("test.db")).unwrap(), before);
        assert_eq!(fs::read(root.path().join("test.db-wal")).unwrap(), wal);
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
    }
    #[test]
    fn key_selection_rejects_duplicate_live_keys_and_other_namespaces() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE keyentry(id INTEGER,alias TEXT,domain INTEGER,namespace INTEGER,state INTEGER,key_type INTEGER); CREATE TABLE blobentry(id INTEGER,keyentryid INTEGER,subcomponent_type INTEGER,blob BLOB); CREATE TABLE keyparameter(keyentryid INTEGER,tag INTEGER,data INTEGER); INSERT INTO keyentry VALUES(1,'sample',0,1000,1,0); INSERT INTO blobentry VALUES(1,1,0,X'1122');").unwrap();
        assert!(select_key(&db, "sample").is_err());
        db.execute_batch("INSERT INTO keyentry VALUES(2,'sample',2,103,1,0); INSERT INTO blobentry VALUES(2,2,0,X'3344');").unwrap();
        assert_eq!(
            select_key(&db, "sample").unwrap().0.as_slice(),
            &[0x33, 0x44]
        );
        db.execute_batch("INSERT INTO keyentry VALUES(3,'sample',2,103,1,0);")
            .unwrap();
        assert!(select_key(&db, "sample").is_err());
    }
    #[test]
    fn unsafe_sources_and_changes_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let mut o = Observed::new(root.path()).unwrap();
        assert!(o.read("../escape", 100, true).is_err());
        fs::write(root.path().join("f"), b"old").unwrap();
        o.required("f", 100).unwrap();
        fs::write(root.path().join("f"), b"new").unwrap();
        assert!(o.unchanged().is_err());
        std::os::unix::fs::symlink(root.path().join("f"), root.path().join("link")).unwrap();
        assert!(o.required("link", 100).is_err());
        assert!(o.required("f", 1).is_err());
    }
    #[test]
    fn input_never_guesses_a_pattern_or_accepts_an_accidental_empty_pin() {
        assert!(validate_input("pin", b"1234").is_ok());
        for bad in [b"".as_slice(), b"123", b"12 34", b"12\n34"] {
            assert!(validate_input("pin", bad).is_err());
        }
        assert!(validate_input("pattern", b"12369").is_err());
        assert!(validate_input("none", b"").is_ok());
        assert!(validate_input("none", b"1234").is_err());
        assert!(validate_input("password", b" a b ").is_ok());
    }
}
