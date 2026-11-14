//! Explicit small HTTP/TLS check with carrier DNS, for device validation.
use anyhow::{ensure, Context, Result};
use std::io::Read;
use std::net::Ipv4Addr;
use std::sync::{Arc,Mutex};
fn main() -> Result<()> {
    let servers: Vec<Ipv4Addr> = std::env::args().skip(1).map(|s| s.parse().context("invalid DNS IPv4")).collect::<Result<_>>()?;
    ensure!(!servers.is_empty(),"supply this data call's DNS servers");
    let reach = skyward_net::reach::probe("http://connect.rom.miui.com/generate_204",&servers);
    println!("HTTP {reach}");
    ensure!(reach == skyward_net::reach::Reach::Online,"HTTP internet check failed");
    let agent = skyward_net::http::agent(Arc::new(Mutex::new(servers)));
    let mut response = agent.get("https://www.baidu.com/robots.txt").call().context("verified TLS request")?;
    ensure!(response.status().is_success(),"HTTPS status {}",response.status());
    let mut bytes = Vec::new();
    response.body_mut().as_reader().take(32769).read_to_end(&mut bytes).context("read TLS response")?;
    ensure!(!bytes.is_empty() && bytes.len() <= 32768,"unexpected HTTPS response size");
    use sha2::{Digest,Sha256};
    println!("HTTPS status={} bytes={} sha256={:x}",response.status(),bytes.len(),Sha256::digest(&bytes));
    Ok(())
}
