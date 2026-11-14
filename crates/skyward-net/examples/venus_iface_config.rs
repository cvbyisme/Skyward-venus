//! Explicit configuration of the diagnostic's own rmnet channel only.
#[cfg(any(target_os = "android", test))]
use anyhow::{ensure,Context,Result};
#[cfg(any(target_os = "android", test))]
use std::net::Ipv4Addr;
#[cfg(any(target_os = "android", test))]
fn main() -> Result<()> {
    ensure!(unsafe { libc::geteuid() }==0 && std::path::Path::new("/skyward/venus-recovery.txt").is_file(),"temporary Venus Recovery only");
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let interface = if args.first().is_some_and(|a| a == "--vendor") { args.remove(0); "rmnet_data0" } else { "sw_cell0" };
    let link = std::fs::read_to_string(format!("/sys/class/net/{interface}/iflink"))?;
    let parent = std::fs::read_to_string("/sys/class/net/rmnet_ipa0/ifindex")?;
    ensure!(link.trim() == parent.trim(), "data channel does not belong to the IPA parent");
    if args.as_slice() == ["--clear"] {
        skyward_net::iface::unconfigure(interface)?;
        skyward_net::iface::set_up(interface,false)?;
        println!("RMNET_IPV4_REMOVED {interface}");
        return Ok(())
    }
    ensure!(args.len()==3,"usage: venus_iface_config [--vendor] IPv4 NETMASK GATEWAY | --clear");
    let parsed: Vec<Ipv4Addr> = args.iter().map(|s| s.parse().context("invalid IPv4 setting")).collect::<Result<_>>()?;
    let (address,mask,gateway)=(parsed[0],parsed[1],parsed[2]);
    let bits=u32::from(mask);let inverse=!bits;
    ensure!(bits!=0 && inverse & inverse.wrapping_add(1)==0,"non-contiguous IPv4 netmask");
    ensure!(!address.is_unspecified() && !address.is_loopback() && !address.is_multicast(),"invalid data address");
    ensure!(u32::from(address)&bits==u32::from(gateway)&bits,"gateway is outside the assigned subnet");
    skyward_net::iface::set_up(interface,true)?;
    skyward_net::iface::configure(interface,address,mask,Some(gateway))?;
    println!("RMNET_IPV4_CONFIGURED {interface}");
    Ok(())
}

#[cfg(not(any(target_os = "android", test)))]
fn main() {
    eprintln!("venus_iface_config is a Venus Android diagnostic");
}
