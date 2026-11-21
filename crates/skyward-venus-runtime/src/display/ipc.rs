//! Privately namespaced display IPC experiment supervisor.  No global Binder
//! service registry, DRM node, whole /data or writable vendor mount is exposed.
//! The original STC/QService clients remain isolated vendor-ABI experiments.
use super::root::{self, Root};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, OsString},
    fs::{self, File},
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, process::CommandExt},
    },
    path::Path,
    process::{Child, Command, ExitStatus},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

const MANAGER_SHA256: &str = "a6ad7a43eb91a5cd87e511f609a3671537eff7e39cfb5bd148f716b0779f9b4d";
const CONTEXTS_SHA256: &str = "e0c155ca1f71d33ce8ff34d719de074d54d241611e2fd588bf4dac99378ea9e4";
static INTERRUPTED: AtomicBool = AtomicBool::new(false);
extern "C" fn stopping(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}
fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}
fn verify_manager(root: &Root) -> Result<()> {
    // Hash the actual immutable copies bound into this root, not a caller path.
    for (file, expected) in [
        ("vendor/bin/vndservicemanager", MANAGER_SHA256),
        ("vendor/etc/selinux/vndservice_contexts", CONTEXTS_SHA256),
    ] {
        let mut source =
            File::open(root.path(file)).with_context(|| format!("manager hash: {file}"))?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let len = source.read(&mut buffer)?;
            if len == 0 {
                break;
            }
            digest.update(&buffer[..len]);
        }
        ensure!(
            format!("{:x}", digest.finalize()) == expected,
            "vendor manager/context hash mismatch: {file}"
        );
    }
    Ok(())
}

fn arguments(args: &[OsString]) -> Result<(&Path, &Path, &Path, &Path, &Path)> {
    ensure!(
        args.len() == 7 && args[4] == "--persist-display" && args[6] == "--list",
        "usage: display-ipc-launch OWNER QSERVICE CLIENT DF_PARAM --persist-display ORIGINAL_RO_DISPLAY_DIR --list"
    );
    for arg in &args[..4] {
        ensure!(
            Path::new(arg).is_absolute(),
            "display artifact path must be absolute"
        );
    }
    let persist = Path::new(&args[5]);
    ensure!(
        persist.is_absolute(),
        "persist display path must be absolute"
    );
    Ok((
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
        Path::new(&args[3]),
        persist,
    ))
}

// The pre-exec hook uses only libc syscalls; in particular, no Rust allocator,
// filesystem traversal, or logging happens between fork and exec.
fn child(
    command: &mut Command,
    parent: libc::pid_t,
    root: Option<&Path>,
    manager: bool,
    ready: Option<i32>,
) -> Result<Child> {
    let directory = root
        .map(|root| CString::new(root.as_os_str().as_bytes()))
        .transpose()?;
    let proc_target = root
        .filter(|_| manager)
        .map(|root| CString::new(root.join("proc").as_os_str().as_bytes()))
        .transpose()?;
    let selinux_target = root
        .filter(|_| manager)
        .map(|root| CString::new(root.join("sys/fs/selinux").as_os_str().as_bytes()))
        .transpose()?;
    command.env_clear().env("PATH", "/system/bin");
    unsafe {
        command.pre_exec(move || {
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGALRM, libc::SIG_DFL);
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 || libc::getppid() != parent
            {
                return Err(io::Error::last_os_error());
            }
            if manager {
                let proc_target = proc_target.as_ref().unwrap();
                let selinux_target = selinux_target.as_ref().unwrap();
                if libc::unshare(libc::CLONE_NEWNS) != 0
                    || libc::mount(
                        std::ptr::null(),
                        c"/".as_ptr(),
                        std::ptr::null(),
                        libc::MS_REC | libc::MS_PRIVATE,
                        std::ptr::null(),
                    ) != 0
                    || libc::mount(
                        c"proc".as_ptr(),
                        proc_target.as_ptr(),
                        c"proc".as_ptr(),
                        libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                        std::ptr::null(),
                    ) != 0
                    || libc::mount(
                        c"/sys/fs/selinux".as_ptr(),
                        selinux_target.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND,
                        std::ptr::null(),
                    ) != 0
                    || libc::mount(
                        std::ptr::null(),
                        selinux_target.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND
                            | libc::MS_REMOUNT
                            | libc::MS_RDONLY
                            | libc::MS_NOSUID
                            | libc::MS_NODEV,
                        std::ptr::null(),
                    ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            if let Some(fd) = ready {
                // Only descriptor 3 survives exec; all other supervisor ends are CLOEXEC.
                if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if let Some(directory) = &directory
                && (libc::chroot(directory.as_ptr()) != 0 || libc::chdir(c"/".as_ptr()) != 0)
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().context("spawn display IPC child")
}
fn wait_exit(c: &mut Child, deadline: Instant) -> Result<Option<ExitStatus>> {
    loop {
        if let Some(status) = c.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(20));
    }
}
fn stop(c: &mut Option<Child>, role: &str, grace: Duration) -> bool {
    let Some(mut child) = c.take() else {
        return true;
    };
    let pid = child.id() as libc::pid_t;
    let early = match child.try_wait() {
        Ok(Some(status)) => return role != "kms-owner" && role != "qservice" || status.success(),
        Ok(None) => false,
        Err(_) => true,
    };
    if early {
        return false;
    }
    eprintln!("DISPLAY_IPC_STOP role={role} pid={pid}");
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let status = wait_exit(&mut child, Instant::now() + grace).ok().flatten();
    if let Some(status) = status {
        return (role != "kms-owner" && role != "qservice") || status.success();
    }
    eprintln!("DISPLAY_IPC_KILL role={role} pid={pid} graceful_cleanup=false");
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    if wait_exit(&mut child, Instant::now() + Duration::from_secs(1))
        .ok()
        .flatten()
        .is_none()
    {
        eprintln!("DISPLAY_IPC_UNREAPED role={role} pid={pid}");
    }
    false
}
#[derive(Default)]
struct Workers {
    owner: Option<Child>,
    manager: Option<Child>,
    service: Option<Child>,
    client: Option<Child>,
}
impl Workers {
    fn cleanup(&mut self) -> bool {
        let _ = stop(&mut self.client, "client", Duration::from_millis(2500));
        let service = stop(&mut self.service, "qservice", Duration::from_millis(2500));
        let manager = stop(
            &mut self.manager,
            "vendor-manager",
            Duration::from_millis(2500),
        );
        let owner = stop(&mut self.owner, "kms-owner", Duration::from_millis(5500));
        service && manager && owner
    }
    fn all_running(&mut self) -> Result<bool> {
        for child in [&mut self.owner, &mut self.manager, &mut self.service] {
            if let Some(child) = child
                && child.try_wait()?.is_some()
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
impl Drop for Workers {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn pipe_ready() -> Result<(File, File)> {
    let mut fds = [-1, -1];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error()).context("qservice readiness pipe");
    }
    Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
}
fn ready(receipt: &mut File, workers: &mut Workers) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        ensure!(
            !interrupted() && Instant::now() < deadline,
            "qservice ready timeout/interrupted"
        );
        ensure!(workers.all_running()?, "IPC child died before readiness");
        let mut token = [0u8; 1];
        match receipt.read(&mut token) {
            Ok(1) if token == *b"R" => return Ok(()),
            Ok(0) => bail!("qservice closed readiness without ACK"),
            Ok(1) => bail!("unrecognized qservice readiness ACK"),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            other => bail!("invalid qservice readiness result: {other:?}"),
        }
        thread::sleep(Duration::from_millis(50));
    }
}

pub fn launch(args: &[OsString]) -> Result<()> {
    let (owner, service, client, param, persist) = arguments(args)?;
    crate::common::root()?;
    INTERRUPTED.store(false, Ordering::Relaxed);
    root::close_extra_fds()?;
    root::persist_checked(persist)?;
    unsafe {
        libc::signal(libc::SIGTERM, stopping as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stopping as *const () as libc::sighandler_t);
    }
    let root = Root::new("ipc", true)?;
    root.file("skyward/runtime/display-ipc-qservice", b"")?;
    root.file("skyward/runtime/display-ipc-client", b"")?;
    root.bind(
        service,
        "skyward/runtime/display-ipc-qservice",
        false,
        false,
    )?;
    root.bind(client, "skyward/runtime/display-ipc-client", false, false)?;
    root.calibration(param, Some(persist))?;
    root.private_binder()?;
    verify_manager(&root)?;

    let run = root::temp_directory("/skyward/run/tmp/venus-display-owner-XXXXXX")?;
    let socket = run.join("owner.sock");
    let parent = unsafe { libc::getpid() };
    let mut workers = Workers::default();
    let outcome = (|| -> Result<()> {
        workers.owner = Some(child(
            Command::new(owner).args([socket.as_os_str(), "--baseline".as_ref()]),
            parent,
            None,
            false,
            None,
        )?);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            ensure!(
                !interrupted() && Instant::now() < deadline,
                "owner did not create IPC socket"
            );
            ensure!(
                workers.owner.as_mut().unwrap().try_wait()?.is_none(),
                "owner exited before socket"
            );
            if fs::symlink_metadata(&socket).is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        root.socket_bind(&socket)?;
        root.seal()?;
        workers.manager = Some(child(
            Command::new("/vendor/bin/vndservicemanager").arg("/dev/vndbinder"),
            parent,
            Some(&root.0),
            true,
            None,
        )?);
        let (mut receipt, writer) = pipe_ready()?;
        let owner_pid = workers.owner.as_ref().unwrap().id();
        let mut qservice = Command::new("/skyward/runtime/display-ipc-qservice");
        qservice
            .arg("/run/owner.sock")
            .arg(owner_pid.to_string())
            .arg("3");
        workers.service = Some(child(
            &mut qservice,
            parent,
            Some(&root.0),
            false,
            Some(writer.as_raw_fd()),
        )?);
        drop(writer);
        ready(&mut receipt, &mut workers)?;
        workers.client = Some(child(
            Command::new("/skyward/runtime/display-ipc-client").arg("--list"),
            parent,
            Some(&root.0),
            false,
            None,
        )?);
        eprintln!(
            "DISPLAY_IPC_STARTED root={} owner_run={} owner={} manager={} qservice={} client={} SELinux=unchanged",
            root.0.display(),
            run.display(),
            owner_pid,
            workers.manager.as_ref().unwrap().id(),
            workers.service.as_ref().unwrap().id(),
            workers.client.as_ref().unwrap().id()
        );
        let deadline = Instant::now() + Duration::from_secs(35);
        loop {
            ensure!(
                !interrupted() && Instant::now() < deadline,
                "IPC display client timed out/interrupted"
            );
            if let Some(status) = workers.client.as_mut().unwrap().try_wait()? {
                ensure!(status.success(), "IPC display client failed: {status}");
                break;
            }
            ensure!(
                workers.all_running()?,
                "display IPC service exited before client"
            );
            thread::sleep(Duration::from_millis(30));
        }
        Ok(())
    })();
    let clean = workers.cleanup();
    if let Err(err) = fs::remove_dir(&run) {
        eprintln!("DISPLAY_IPC_OWNER_DIR_REMAINS {}: {err}", run.display());
    }
    let success = outcome.is_ok() && clean;
    eprintln!(
        "DISPLAY_IPC_FINISHED success={} root={} remove_empty_root_after_exit=true",
        u8::from(success),
        root.0.display()
    );
    outcome?;
    ensure!(clean, "display IPC cleanup did not finish cleanly");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_ipc_arguments() {
        let good: Vec<OsString> = [
            "/x/owner",
            "/x/qservice",
            "/x/client",
            "/x/config",
            "--persist-display",
            "/x/persist",
            "--list",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        assert!(arguments(&good).is_ok());
        for change in [
            (4, "--no-persist"),
            (5, "relative"),
            (6, "--render"),
            (0, "owner"),
        ] {
            let mut bad = good.clone();
            bad[change.0] = change.1.into();
            assert!(arguments(&bad).is_err());
        }
        assert!(arguments(&good[..6]).is_err());
    }
    #[test]
    fn pinned_manager_hashes_are_full_sha256() {
        for value in [MANAGER_SHA256, CONTEXTS_SHA256] {
            assert!(value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }
}
