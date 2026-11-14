//! State shared by the independent Venus modem owner and its UI client.
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use crate::cellular_data::{PacketState, Settings};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Off,
    Starting,
    Ready,
    Stopping,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub boot_id: String,
    pub owner: u32,
    pub command: String,
    pub radio: Phase,
    pub data: Phase,
    pub address: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub issue: Option<String>,
    /// Last actual WDS response, not inferred from native interface state.
    /// Older snapshots remain readable; absence never establishes readiness.
    #[serde(default)]
    pub packet_state: Option<PacketState>,
}

impl State {
    /// Publish a still-owned call after its native IP and owned route were
    /// verified. Pauses keep the data intent on but are not internet readiness.
    pub fn active_packet(&mut self, packet: PacketState, settings: &Settings) -> anyhow::Result<()> {
        anyhow::ensure!(packet != PacketState::Disconnected, "disconnected call needs verified teardown before publication");
        self.packet_state = Some(packet);
        self.data = Phase::Ready;
        match packet {
            PacketState::Connected => {
                self.address = Some(settings.address); self.dns = settings.dns.clone(); self.issue = None;
            },
            PacketState::Suspended | PacketState::Authenticating => {
                self.address = None; self.dns.clear();
                self.issue = Some(match packet {
                    PacketState::Suspended => "Mobile data is suspended; the call and route are retained.",
                    _ => "Mobile data is authenticating; the call and route are retained.",
                }.into());
            },
            PacketState::Disconnected => unreachable!(),
        }
        Ok(())
    }
}

/// WLAN has the preferred default route. Never keep its old DNS after
/// losing the lease, or use cellular DNS while WLAN owns the route.
pub fn dns(wlan: Option<&crate::wlan::Link>, modem: Option<&State>) -> Vec<Ipv4Addr> {
    if let Some(link) = wlan { return link.dns.clone(); }
    modem.filter(|s| s.radio == Phase::Ready && s.data == Phase::Ready
        && s.packet_state == Some(PacketState::Connected) && s.address.is_some())
        .map(|s| s.dns.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cellular_dns_is_removed_when_its_call_is_not_ready() {
        let mut s=State { radio:Phase::Ready,data:Phase::Ready,address:Some(Ipv4Addr::new(10,1,2,3)),
            packet_state:Some(PacketState::Connected),dns:vec![Ipv4Addr::new(1,2,3,4)],..Default::default() };
        assert_eq!(dns(None,Some(&s)),s.dns);
        for phase in [Phase::Off,Phase::Starting,Phase::Stopping,Phase::Failed] {
            s.data=phase;assert!(dns(None,Some(&s)).is_empty());
        }
        s.data=Phase::Ready;
        for state in [None,Some(PacketState::Disconnected),Some(PacketState::Suspended),Some(PacketState::Authenticating)] {
            s.packet_state=state;assert!(dns(None,Some(&s)).is_empty(),"stale DNS cannot imply a ready packet call");
        }
    }

    #[test]
    fn suspended_and_authenticating_keep_the_switch_on_without_publishing_internet_readiness() {
        let settings=Settings { address:Ipv4Addr::new(10,1,2,3),netmask:Ipv4Addr::new(255,255,255,0),
            gateway:Ipv4Addr::new(10,1,2,1),mtu:1500,dns:vec![Ipv4Addr::new(1,2,3,4)] };
        let mut s=State { radio:Phase::Ready,..Default::default() };
        s.active_packet(PacketState::Connected,&settings).unwrap();
        assert_eq!(dns(None,Some(&s)),settings.dns);
        for packet in [PacketState::Suspended,PacketState::Authenticating] {
            s.active_packet(packet,&settings).unwrap();
            assert_eq!(s.data,Phase::Ready,"the explicit data intent remains enabled");
            assert_eq!(s.packet_state,Some(packet));
            assert!(s.address.is_none() && s.dns.is_empty() && s.issue.is_some());
            assert!(dns(None,Some(&s)).is_empty());
            s.active_packet(PacketState::Connected,&settings).unwrap();
            assert_eq!(s.address,Some(settings.address));
            assert_eq!(dns(None,Some(&s)),settings.dns);
            assert!(s.issue.is_none());
        }
        let before=s.clone();
        assert!(s.active_packet(PacketState::Disconnected,&settings).is_err());
        assert_eq!(s,before,"a disconnect cannot bypass verified teardown");
        let mut old=serde_json::to_value(&s).unwrap();
        old.as_object_mut().unwrap().remove("packet_state");
        let old:State=serde_json::from_value(old).unwrap();
        assert!(old.packet_state.is_none() && dns(None,Some(&old)).is_empty());
    }
}
