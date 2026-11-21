//! Cooperative ownership of terminal and installer descendants. These RAM
//! receipts survive a UI exit; they are not a sandbox for privileged scripts.
//! No runtime daemon or global maintenance lease is acquired here.

use anyhow::{Context, Result, bail, ensure};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

const DIRECTORY: &str = "/skyward/run/tasks";
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    TerminalSession,
    InstallerGroup,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::TerminalSession => "terminal-session",
            Self::InstallerGroup => "installer-group",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Process {
    pid: i32,
    state: u8,
    group: i32,
    session: i32,
    start: u64,
}

fn parse_stat(raw: &str) -> Result<Process> {
    let (pid, _) = raw.split_once(' ').context("missing process PID")?;
    let (_, tail) = raw
        .rsplit_once(") ")
        .context("missing process stat fields")?;
    let fields: Vec<_> = tail.split_whitespace().collect();
    ensure!(
        fields.len() >= 20 && fields[0].len() == 1,
        "short process stat"
    );
    Ok(Process {
        pid: pid.parse()?,
        state: fields[0].as_bytes()[0],
        group: fields[2].parse()?,
        session: fields[3].parse()?,
        start: fields[19].parse()?,
    })
}

fn process(pid: i32) -> Result<Option<Process>> {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(raw) => Ok(Some(parse_stat(&raw)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("read owned process identity"),
    }
}

fn members(kind: Kind, id: i32) -> Result<Vec<Process>> {
    let mut result = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        let Some(p) = process(pid)? else { continue };
        let matches = match kind {
            Kind::TerminalSession => p.session == id,
            Kind::InstallerGroup => p.group == id,
        };
        if matches && !matches!(p.state, b'Z' | b'X' | b'x') {
            result.push(p);
        }
    }
    Ok(result)
}

fn enabled() -> bool {
    Path::new("/skyward/venus-recovery.txt").is_file()
}

fn checked_directory(path: &Path, create: bool) -> Result<bool> {
    if create {
        match fs::create_dir(path) {
            Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(0o700))?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e).context("create task receipt directory"),
        }
    }
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).context("inspect task receipt directory"),
    };
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "unsafe task receipt directory"
    );
    Ok(true)
}

struct Receipt {
    path: PathBuf,
    file: File,
    boot: String,
    kind: Kind,
}

impl Receipt {
    fn reserve(root: &Path, kind: Kind) -> Result<Self> {
        checked_directory(root, true)?;
        let boot = fs::read_to_string(BOOT_ID)?.trim().to_owned();
        let nonce = fs::read_to_string("/proc/sys/kernel/random/uuid")?;
        ensure!(
            nonce
                .trim()
                .bytes()
                .all(|c| c.is_ascii_hexdigit() || c == b'-'),
            "invalid task nonce"
        );
        let path = root.join(format!("{}.task", nonce.trim()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)?;
        let mut receipt = Self {
            path,
            file,
            boot,
            kind,
        };
        receipt.write(None)?;
        Ok(receipt)
    }
    fn write(&mut self, owner: Option<Process>) -> Result<()> {
        let identity = owner.map_or_else(
            || "pending".to_owned(),
            |p| format!("{} {}", p.pid, p.start),
        );
        let raw = format!(
            "skyward-task-v1\n{}\n{}\n{}\n",
            self.boot,
            self.kind.name(),
            identity
        );
        self.file.rewind()?;
        self.file.set_len(0)?;
        self.file.write_all(raw.as_bytes())?;
        Ok(())
    }
    fn remove(&self) -> Result<()> {
        let held = self.file.metadata()?;
        let named = fs::symlink_metadata(&self.path)?;
        ensure!(
            (held.dev(), held.ino()) == (named.dev(), named.ino()) && named.nlink() == 1,
            "task receipt replaced"
        );
        fs::remove_file(&self.path)?;
        Ok(())
    }
}

/// The direct child is kept separately by its existing owner. Dropping this
/// object never signals a numeric PID/group after the child may have been reaped.
pub struct Task {
    kind: Kind,
    owner: Process,
    receipt: Option<Receipt>,
}

impl Task {
    pub fn spawn(command: &mut Command, kind: Kind) -> Result<(Child, Self)> {
        Self::spawn_at(command, kind, enabled().then(|| Path::new(DIRECTORY)))
    }
    fn spawn_at(command: &mut Command, kind: Kind, root: Option<&Path>) -> Result<(Child, Self)> {
        // Write a pending receipt before fork. An interrupted handoff remains
        // unresolved instead of disappearing when UI exits unexpectedly.
        let mut receipt = root.map(|root| Receipt::reserve(root, kind)).transpose()?;
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                if let Some(receipt) = receipt {
                    receipt.remove()?;
                }
                return Err(e).context("start tracked task");
            }
        };
        let attached = (|| {
            let owner = process(child.id() as i32)?
                .context("task leader disappeared before ownership was recorded")?;
            let id = match kind {
                Kind::TerminalSession => owner.session,
                Kind::InstallerGroup => owner.group,
            };
            ensure!(id == owner.pid, "task did not enter its own session/group");
            if let Some(receipt) = &mut receipt {
                receipt.write(Some(owner))?;
            }
            Ok(owner)
        })();
        match attached {
            Ok(owner) => Ok((
                child,
                Self {
                    kind,
                    owner,
                    receipt,
                },
            )),
            Err(e) => {
                // Never remove the pending receipt after a child was created.
                let _ = child.kill();
                let until = Instant::now() + STOP_TIMEOUT;
                while Instant::now() < until {
                    if child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e)
            }
        }
    }
    fn live(&self) -> Result<Vec<Process>> {
        if let Some(current) = process(self.owner.pid)? {
            ensure!(
                current.start == self.owner.start,
                "task leader PID was reused; ownership is unresolved"
            );
        }
        members(self.kind, self.owner.pid)
    }
    /// Called only with our still-unreaped direct child. Keep that leader until
    /// signals have been sent, so the process-group ID cannot name a new group.
    pub fn stop_installer(&mut self, child: &mut Child) -> Result<()> {
        ensure!(
            self.kind == Kind::InstallerGroup && child.id() as i32 == self.owner.pid,
            "installer child/group mismatch"
        );
        let current = process(self.owner.pid)?
            .context("installer leader was reaped before group shutdown")?;
        ensure!(
            current.start == self.owner.start && current.group == self.owner.pid,
            "installer identity changed"
        );
        let signalled = unsafe { libc::kill(-self.owner.pid, libc::SIGKILL) };
        ensure!(
            signalled == 0,
            "signal installer group: {}",
            std::io::Error::last_os_error()
        );
        let until = Instant::now() + STOP_TIMEOUT;
        loop {
            let live = self.live()?;
            if live.is_empty() {
                // Only Child::try_wait reaps, and only our direct child.
                if child.try_wait()?.is_some() {
                    if let Some(receipt) = self.receipt.take() {
                        receipt.remove()?;
                    }
                    return Ok(());
                }
            }
            if Instant::now() >= until {
                // The last signal has already been sent. Reap the direct
                // leader if it died, even when another member remains stuck.
                let _ = child.try_wait()?;
                bail!(
                    "installer group {} has not exited within 2 seconds; retained task receipt (live PIDs: {:?})",
                    self.owner.pid,
                    live.iter().map(|p| p.pid).collect::<Vec<_>>()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        if let Some(receipt) = &self.receipt {
            match self.live() {
                Ok(live) if live.is_empty() => {
                    let _ = receipt.remove();
                }
                _ => (), // Retained for maintenance's independent check.
            }
        }
    }
}

/// Observe exit without reaping: the installer needs the leader's identity to
/// remain reserved until the entire group has been stopped.
pub fn observed_exit(child: &Child) -> Result<Option<ExitStatus>> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("observe task exit without reaping");
    }
    if unsafe { info.si_pid() } == 0 {
        return Ok(None);
    }
    let status = unsafe { info.si_status() };
    let raw = match info.si_code {
        libc::CLD_EXITED => status << 8,
        libc::CLD_KILLED => status,
        libc::CLD_DUMPED => status | 0x80,
        _ => bail!("unexpected task wait status"),
    };
    Ok(Some(ExitStatus::from_raw(raw)))
}

/// No lease or daemon-start policy is changed. Only maintenance calls this;
/// ordinary service readiness queries remain free to run with a live UI.
pub fn ensure_quiet() -> Result<()> {
    if enabled() {
        ensure_quiet_at(Path::new(DIRECTORY))?;
    }
    Ok(())
}

/// An unsuccessful script may still own mounts and write partitions. Its
/// cleanup/reflash must also wait for confirmed exit, independently of PTYs.
pub fn ensure_installers_quiet() -> Result<()> {
    if enabled() {
        check_receipts(Path::new(DIRECTORY), Some(Kind::InstallerGroup))?;
    }
    Ok(())
}

fn ensure_quiet_at(root: &Path) -> Result<()> {
    check_receipts(root, None)
}

fn check_receipts(root: &Path, only_kind: Option<Kind>) -> Result<()> {
    if !checked_directory(root, false)? {
        return Ok(());
    }
    let boot = fs::read_to_string(BOOT_ID)?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        let meta = file.metadata()?;
        ensure!(
            meta.is_file()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.mode() & 0o077 == 0
                && meta.nlink() == 1
                && meta.len() <= 1024,
            "unsafe task receipt"
        );
        let mut raw = String::new();
        file.read_to_string(&mut raw)?;
        let lines: Vec<_> = raw.lines().collect();
        ensure!(
            lines.len() == 4 && lines[0] == "skyward-task-v1" && lines[1] == boot.trim(),
            "stale or incomplete task receipt"
        );
        let kind = match lines[2] {
            "terminal-session" => Kind::TerminalSession,
            "installer-group" => Kind::InstallerGroup,
            _ => bail!("unknown task receipt kind"),
        };
        if only_kind.is_some_and(|wanted| wanted != kind) {
            continue;
        }
        ensure!(
            lines[3] != "pending",
            "a {} was interrupted before its process identity was recorded; restart Recovery before maintenance",
            kind.name()
        );
        let (pid, start) = lines[3]
            .split_once(' ')
            .context("invalid task receipt identity")?;
        let pid = pid.parse::<i32>()?;
        let start = start.parse::<u64>()?;
        ensure!(pid > 1 && start > 0, "invalid task receipt identity");
        if let Some(current) = process(pid)? {
            ensure!(
                current.start == start,
                "previous {} PID was reused; ownership is unresolved",
                kind.name()
            );
        }
        let live = members(kind, pid)?;
        ensure!(
            live.is_empty(),
            "previous {} {} still has live background tasks (PIDs: {:?}); wait for them to exit before maintenance",
            kind.name(),
            pid,
            live.iter().map(|p| p.pid).collect::<Vec<_>>()
        );
        let receipt = Receipt {
            path,
            file,
            boot: boot.trim().to_owned(),
            kind,
        };
        receipt.remove()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    fn wait_for<T>(mut get: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(value) = get() {
                return value;
            }
            assert!(Instant::now() < deadline, "fixture deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn background_pid(path: &Path) -> i32 {
        wait_for(|| fs::read_to_string(path).ok()?.trim().parse().ok())
    }

    fn reap_adopted(pid: i32) {
        // Only this isolated fixture is a subreaper. Never put this adoption
        // policy in the UI, which owns unrelated daemon children as well.
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    }

    #[test]
    fn real_background_tasks_are_tracked_and_stopped_in_isolated_process() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "task_scope::tests::subprocess_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("SKYWARD_TASK_SCOPE_FIXTURE", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn subprocess_fixture() {
        if std::env::var("SKYWARD_TASK_SCOPE_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
            0
        );
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("receipts");
        let pidfile = temp.path().join("background-pid");

        // A normal job-control shell creates another PGID in the same SID.
        let mut command = Command::new("/bin/bash");
        command
            .args([
                "--noprofile",
                "--norc",
                "-c",
                "set -m; sleep 30 </dev/null >/dev/null 2>&1 & echo $! > \"$1\"; exit 0",
                "fixture",
            ])
            .arg(&pidfile)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let (mut leader, ticket) =
            Task::spawn_at(&mut command, Kind::TerminalSession, Some(&root)).unwrap();
        let background = background_pid(&pidfile);
        assert!(leader.wait().unwrap().success());
        let child = process(background).unwrap().unwrap();
        assert_eq!(child.session, leader.id() as i32);
        assert_ne!(child.group, leader.id() as i32);
        drop(ticket); // Same as UI's shell = None; receipt must survive it.
        let refused = ensure_quiet_at(&root).unwrap_err().to_string();
        assert!(
            refused.contains("terminal-session") && refused.contains(&background.to_string()),
            "{refused}"
        );
        // An unrelated terminal must not prevent completed script cleanup.
        check_receipts(&root, Some(Kind::InstallerGroup)).unwrap();
        assert_eq!(unsafe { libc::kill(background, libc::SIGKILL) }, 0);
        reap_adopted(background);
        ensure_quiet_at(&root).unwrap();
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);

        fs::remove_file(&pidfile).unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "sleep 30 </dev/null >/dev/null 2>&1 & echo $! > \"$1\"; exit 0",
                "fixture",
            ])
            .arg(&pidfile)
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let (mut leader, mut ticket) =
            Task::spawn_at(&mut command, Kind::InstallerGroup, Some(&root)).unwrap();
        let background = background_pid(&pidfile);
        assert!(wait_for(|| observed_exit(&leader).unwrap()).success());
        assert_eq!(process(leader.id() as i32).unwrap().unwrap().state, b'Z'); // identity still reserved
        let mut output = String::new();
        leader
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .unwrap();
        assert!(output.is_empty()); // Closed output did not mean the child ended.
        assert_eq!(
            process(background).unwrap().unwrap().group,
            leader.id() as i32
        );
        // Script cleanup/reflash uses this same check. Even after leader exit
        // and EOF it must refuse while the ordinary background child lives.
        assert!(check_receipts(&root, Some(Kind::InstallerGroup)).is_err());
        let started = Instant::now();
        ticket.stop_installer(&mut leader).unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(process(leader.id() as i32).unwrap().is_none());
        let zombie = process(background).unwrap().unwrap();
        assert!(matches!(zombie.state, b'Z' | b'X' | b'x'));
        ensure_quiet_at(&root).unwrap(); // The owner did not reap its grandchild.
        reap_adopted(background);
    }

    #[test]
    fn interrupted_spawn_and_reused_identity_never_look_quiet() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("receipts");
        let mut receipt = Receipt::reserve(&root, Kind::TerminalSession).unwrap();
        assert!(
            ensure_quiet_at(&root)
                .unwrap_err()
                .to_string()
                .contains("before its process identity")
        );
        let mut identity = process(std::process::id() as i32).unwrap().unwrap();
        identity.start += 1;
        receipt.write(Some(identity)).unwrap();
        assert!(
            ensure_quiet_at(&root)
                .unwrap_err()
                .to_string()
                .contains("PID was reused")
        );
        receipt.remove().unwrap();
        ensure_quiet_at(&root).unwrap();
    }

    #[test]
    fn failed_exec_removes_only_its_pending_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("receipts");
        let result = Task::spawn_at(
            &mut Command::new("/definitely-not-a-skyward-program"),
            Kind::InstallerGroup,
            Some(&root),
        );
        assert!(result.is_err());
        ensure_quiet_at(&root).unwrap();
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }
}
