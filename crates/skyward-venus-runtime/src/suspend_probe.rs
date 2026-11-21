//! Opt-in device diagnostics. Only the service writes wake locks; this client
//! observes sysfs and never asks the kernel to suspend or signals other owners.
use crate::suspend::{LOCK, SUSPEND};
use anyhow::{Context, Result, ensure};
use skyward_binder::{
    driver::{Binder, Handle},
    hidl::{self, Message},
    hidl_serve::BASE,
};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    io::{self, Write},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
type Tokens = BTreeSet<String>;
static STOPPED: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOPPED.store(true, Ordering::Relaxed);
}
fn active(prefix: &str) -> Result<Tokens> {
    Ok(std::fs::read_to_string("/sys/power/wake_lock")?
        .split_whitespace()
        .filter(|n| n.starts_with(prefix))
        .map(str::to_owned)
        .collect())
}
fn describe(tokens: &Tokens) -> String {
    if tokens.is_empty() {
        "<empty>".into()
    } else {
        tokens.iter().cloned().collect::<Vec<_>>().join(",")
    }
}
fn await_exact(prefix: &str, expected: &Tokens, phase: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let observed = active(prefix)?;
        if observed == *expected {
            println!("PASS phase={phase} tokens={}", describe(&observed));
            io::stdout().flush()?;
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "{phase}: expected={} observed={}",
            describe(expected),
            describe(&observed)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn acquire<'a>(service: &Handle<'a>, name: &str) -> Result<Handle<'a>> {
    let mut call = Message::call(SUSPEND);
    call.u32(0);
    call.text(name);
    let reply = service.transact(1, &call)?;
    let mut read = reply.hidl();
    read.status()?;
    let handle = read
        .object()?
        .context("acquire returned a successful null lock")?;
    read.finish()?;
    Handle::hold(reply.binder(), handle)
}
fn observed_acquire<'a>(
    service: &Handle<'a>,
    prefix: &str,
    expected: &mut Tokens,
    name: &str,
) -> Result<(Handle<'a>, String)> {
    await_exact(prefix, expected, "before-acquire")?;
    let lock = acquire(service, name)?;
    let observed = active(prefix)?;
    let added: Vec<_> = observed.difference(expected).cloned().collect();
    ensure!(
        added.len() == 1 && observed.len() == expected.len() + 1,
        "acquire did not add one independent token: {}",
        describe(&observed)
    );
    let token = added[0].clone();
    expected.insert(token.clone());
    println!("ACQUIRED token={token} debug_name={name}");
    io::stdout().flush()?;
    Ok((lock, token))
}
fn service(binder: &Binder) -> Result<Handle<'_>> {
    hidl::find(binder, SUSPEND, "default")?.context("HIDL suspend service is not registered")
}
fn exercise(prefix: &str) -> Result<()> {
    let binder = Binder::hardware()?;
    let service = service(&binder)?;
    let baseline = active(prefix)?;
    let mut expected = baseline.clone();
    let (guard, guard_token) =
        observed_acquire(&service, prefix, &mut expected, "skyward-probe-guard")?;
    let name = format!("skyward probe same name {}", std::process::id());
    let (first, first_token) = observed_acquire(&service, prefix, &mut expected, &name)?;
    let (second, second_token) = observed_acquire(&service, prefix, &mut expected, &name)?;
    ensure!(
        first_token != second_token && first_token != guard_token && second_token != guard_token,
        "shared kernel token"
    );
    first.oneway(1, &Message::call(LOCK))?;
    expected.remove(&first_token);
    await_exact(
        prefix,
        &expected,
        "release-first-preserves-second-and-guard",
    )?;
    first.oneway(1, &Message::call(LOCK))?;
    first
        .transact(256_921_159, &Message::call(BASE))?
        .hidl()
        .status()?;
    for _ in 0..15 {
        std::thread::sleep(Duration::from_millis(20));
        ensure!(
            active(prefix)? == expected,
            "duplicate release changed another token"
        );
    }
    await_exact(prefix, &expected, "duplicate-release-is-idempotent")?;
    drop(first);
    drop(second);
    expected.remove(&second_token);
    await_exact(prefix, &expected, "last-reference-drop-preserves-guard")?;
    drop(guard);
    await_exact(
        prefix,
        &baseline,
        "all-probe-tokens-gone-baseline-preserved",
    )
}
fn client_exit(prefix: &str) -> Result<()> {
    let baseline = active(prefix)?;
    // No Binder connection or worker has been created when fork occurs.
    let child = unsafe { libc::fork() };
    ensure!(child >= 0, "fork: {}", io::Error::last_os_error());
    if child == 0 {
        unsafe {
            libc::alarm(12);
        }
        let result = (|| -> Result<()> {
            let binder = Binder::hardware()?;
            let service = service(&binder)?;
            let mut expected = baseline.clone();
            let (a, at) = observed_acquire(
                &service,
                prefix,
                &mut expected,
                "skyward-probe-client-exit-same-name",
            )?;
            let (b, bt) = observed_acquire(
                &service,
                prefix,
                &mut expected,
                "skyward-probe-client-exit-same-name",
            )?;
            await_exact(prefix, &expected, "client-holds-two-before-exit")?;
            println!(
                "CLIENT_EXIT pid={} tokens={at},{bt} destructors=false",
                std::process::id()
            );
            io::stdout().flush()?;
            // Keep objects alive until _exit so the kernel must do cleanup.
            std::mem::forget((a, b));
            unsafe { libc::_exit(0) }
        })();
        if let Err(e) = result {
            eprintln!("FAIL client-exit-child: {e:#}");
        }
        unsafe { libc::_exit(1) }
    }
    let mut status = 0;
    loop {
        let ret = unsafe { libc::waitpid(child, &mut status, 0) };
        if ret == child {
            break;
        }
        if ret < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        anyhow::bail!("wait for probe child");
    }
    ensure!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "client-exit child failed"
    );
    await_exact(
        prefix,
        &baseline,
        "client-process-exit-releases-both-tokens",
    )
}
fn hold(prefix: &str, pid: u32) -> Result<()> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = stop as *const () as usize;
    ensure!(
        unsafe { libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) } == 0,
        "install hold SIGTERM handler"
    );
    let binder = Binder::hardware()?;
    let service = service(&binder)?;
    let name = format!("skyward-probe-hold-same-name-{}", std::process::id());
    let a = acquire(&service, &name)?;
    let b = acquire(&service, &name)?;
    let observed = active(prefix)?;
    ensure!(
        !a.same_object(&b) && observed.len() >= 2,
        "hold requires distinct objects and at least two bridge tokens"
    );
    ensure!(
        !STOPPED.load(Ordering::Relaxed),
        "hold cancelled before READY"
    );
    println!(
        "VENUS_SUSPEND_PROBE_HOLD_READY pid={} service_pid={pid} held_objects=2 bridge_active_tokens={} token_attribution=not_asserted",
        std::process::id(),
        observed.len()
    );
    io::stdout().flush()?;
    while !STOPPED.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
    }
    println!(
        "VENUS_SUSPEND_PROBE_HOLD_STOP pid={} reason=SIGTERM",
        std::process::id()
    );
    io::stdout().flush()?;
    Ok(())
}
pub fn run(args: &[OsString]) -> Result<()> {
    ensure!(
        args.len() == 2,
        "usage: network-suspend-probe SERVICE_PID exercise|client-exit|hold"
    );
    let pid: u32 = args[0].to_str().context("PID UTF8")?.parse()?;
    ensure!(pid > 1 && pid <= i32::MAX as u32, "invalid service PID");
    let mode = args[1].to_str().context("mode UTF8")?;
    let prefix = format!("skyward-suspend-{pid}-");
    unsafe {
        libc::alarm(20);
    }
    match mode {
        "exercise" => exercise(&prefix)?,
        "client-exit" => client_exit(&prefix)?,
        "hold" => hold(&prefix, pid)?,
        _ => anyhow::bail!("invalid probe mode"),
    };
    unsafe {
        libc::alarm(0);
    }
    println!("PASS mode={mode} prefix={prefix} sysfs_writes=none suspend_requests=none");
    Ok(())
}
