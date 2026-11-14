//! Explicit scan through the same control protocol as the official WLAN UI.
#[cfg(target_os = "android")]
fn main() -> anyhow::Result<()> {
    use anyhow::{ensure, Context};
    use skyward_net::supplicant::{self, Control, Event};
    use std::path::Path;
    use std::time::{Duration, Instant};
    ensure!(unsafe { libc::geteuid() } == 0 && Path::new("/skyward/venus-recovery.txt").is_file(), "temporary Venus Recovery only");
    let mode = std::env::args().nth(1).context("usage: venus_wlan_probe status|scan")?;
    ensure!(matches!(mode.as_str(), "status" | "scan"), "unsupported probe mode");
    let path = Path::new("/skyward/run/supplicant/sockets/wlan0");
    let control = Control::open(path)?;
    ensure!(control.ask("PING")?.trim() == "PONG", "supplicant did not answer PING");
    println!("PING PONG\n{}", control.ask("STATUS")?);
    if mode == "status" { return Ok(()) }
    let monitor = Control::open(path)?;
    monitor.attach()?;
    control.order("SCAN")?;
    let until = Instant::now() + Duration::from_secs(30);
    'wait: loop {
        ensure!(Instant::now() < until, "scan did not complete in 30 seconds");
        for message in monitor.messages()? {
            match supplicant::event(&message) {
                Some(Event::ScanResults) => break 'wait,
                Some(Event::ScanFailed) => anyhow::bail!("supplicant reported a failed scan"),
                Some(Event::Terminating) => anyhow::bail!("supplicant ended"),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut first = 0;
    let mut count = 0;
    loop {
        let page = supplicant::bss_list(&control.ask(&supplicant::bss_command(first))?)?;
        let Some(last) = page.last() else { break };
        ensure!(last.id >= first, "supplicant did not advance its BSS list");
        first = last.id.checked_add(1).context("BSS index overflow")?;
        for bss in &page {
            println!("{}", serde_json::json!({"ssid":String::from_utf8_lossy(&bss.ssid),"frequency":bss.frequency,"signal_dbm":bss.signal,"security":format!("{:?}",bss.security)}));
        }
        count += page.len();
        ensure!(count <= 4096, "too many access points");
    }
    println!("WLAN_SCAN_COMPLETE access_points={count}");
    Ok(())
}
#[cfg(not(target_os = "android"))]
fn main() { eprintln!("This diagnostic requires temporary Venus Recovery."); }
