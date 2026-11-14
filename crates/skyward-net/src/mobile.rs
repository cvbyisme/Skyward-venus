//! The live modem readings used by the SIM page and status bar.
use crate::qmi::{Card, OperatingMode, ServingSystem, Signal};

/// Whether the public modem readings can be used. SIM absence is a valid
/// reading inside `cards`, never a substitute for a failed query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness { Checking, Stopped, Unavailable, Failed, Partial, Ready }

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mobile {
    pub checking: bool,
    pub available: bool,
    pub cards: Option<Vec<Card>>,
    pub network: Option<ServingSystem>,
    pub signals: Vec<Signal>,
    pub power: Option<OperatingMode>,
    pub incomplete: bool,
    /// Whether the owned radio has been started. `None` means its owner
    /// has not yet published a state, rather than that the SIM is absent.
    pub modem_started: Option<bool>,
    pub modem: Option<crate::modem::State>,
    /// Query failures, without subscriber identifiers or authentication
    /// material. Successful fields remain usable after a partial failure.
    pub issue: Option<String>,
}
impl Mobile {
    pub fn readiness(&self) -> Readiness {
        if let Some(modem) = &self.modem {
            match modem.radio {
                crate::modem::Phase::Off => return Readiness::Stopped,
                crate::modem::Phase::Starting | crate::modem::Phase::Stopping => return Readiness::Checking,
                crate::modem::Phase::Failed => return Readiness::Failed,
                crate::modem::Phase::Ready => {}
            }
        }
        if self.checking { Readiness::Checking }
        else if self.available {
            if self.incomplete || self.issue.is_some() { Readiness::Partial } else { Readiness::Ready }
        } else if self.modem_started == Some(false) { Readiness::Stopped }
        else if self.issue.is_some() { Readiness::Failed }
        else { Readiness::Unavailable }
    }
    pub fn registered(&self) -> bool { self.network.as_ref().is_some_and(|n| n.registration_state == 1) }
    pub fn radio(&self) -> Option<u8> {
        self.network.as_ref()?.radio_interfaces.iter().rev().copied().find(|r| matches!(r,1|2|4|5|8|9|12))
    }
    pub fn technology(&self) -> &'static str {
        match self.radio() { Some(12) => "5G",Some(8) => "4G",Some(5|9) => "3G",Some(4) => "2G",Some(1|2) => "CDMA",_ => "—" }
    }
    /// Select only a measurement for the registered technology. In
    /// particular an unavailable NR value must not borrow LTE's bars.
    pub fn measured(&self) -> Option<(&'static str,i16)> {
        if !self.registered() { return None }
        let radio = self.radio()?;
        let s = self.signals.iter().find(|s| s.radio == radio)?;
        match radio {
            8 => s.reference_dbm.filter(|d| (-140..=-44).contains(d)).map(|d| ("RSRP",d)),
            12 => s.reference_dbm.filter(|d| (-156..=-31).contains(d)).map(|d| ("RSRP",d)),
            5 => s.reference_dbm.filter(|d| (-120..=-24).contains(d)).map(|d| ("RSCP",d))
                .or_else(|| s.rssi_dbm.filter(|d| (-113..=-51).contains(d)).map(|d| ("RSSI",d))),
            _ => s.rssi_dbm.filter(|d| (-113..=-51).contains(d)).map(|d| ("RSSI",d)),
        }
    }
    pub fn level(&self) -> Option<u8> {
        let (kind,dbm) = self.measured()?;
        // AOSP CellSignalStrength{Lte,Nr} defaults, not a carrier override.
        // https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/telephony/java/android/telephony/CellSignalStrengthLte.java
        // https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/telephony/java/android/telephony/CellSignalStrengthNr.java
        let thresholds = match (self.radio(),kind) {
            (Some(12),_) => [-110,-90,-80,-65],
            (Some(8),_) => [-115,-105,-95,-85],
            (_,"RSCP") => [-115,-105,-95,-85],
            _ => [-113,-103,-97,-89],
        };
        Some(thresholds.iter().filter(|&&v| dbm >= v).count() as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registered(radio:u8) -> Mobile {
        Mobile { available:true, network:Some(ServingSystem { registration_state:1,circuit_attach_state:2,packet_attach_state:1,
            selected_network:0,radio_interfaces:vec![radio],plmn:None,roaming:None }),..Mobile::default() }
    }
    #[test]
    fn missing_nr_measurement_does_not_show_lte_bars_or_no_sim() {
        let mut m = registered(12);
        m.signals = vec![Signal { radio:8,rssi_dbm:Some(-67),reference_dbm:Some(-95) },
            Signal { radio:12,rssi_dbm:None,reference_dbm:Some(i16::MIN) }];
        assert_eq!(m.technology(),"5G"); assert!(m.registered());
        assert_eq!(m.cards,None); assert_eq!(m.level(),None);
        m.network.as_mut().unwrap().radio_interfaces = vec![8];
        assert_eq!(m.level(),Some(3));
    }
    #[test]
    fn old_signal_is_not_shown_after_registration_is_lost() {
        let mut m=registered(8);
        m.signals.push(Signal { radio:8,rssi_dbm:Some(-67),reference_dbm:Some(-85) });
        assert_eq!(m.level(),Some(4));
        m.network.as_mut().unwrap().registration_state=2;
        assert_eq!(m.measured(),None); assert_eq!(m.level(),None);
    }
    #[test]
    fn modem_not_started_failed_query_and_no_cards_are_distinct() {
        let mut m = Mobile::default();
        assert_eq!(m.readiness(),Readiness::Unavailable);
        m.modem_started = Some(false);
        assert_eq!(m.readiness(),Readiness::Stopped);
        m.modem_started = Some(true);
        m.issue = Some("SIM status request timed out".into());
        assert_eq!(m.readiness(),Readiness::Failed);
        assert_eq!(m.cards,None);
        m.available = true;
        m.cards = Some(Vec::new());
        assert_eq!(m.readiness(),Readiness::Partial);
        m.issue = None;
        assert_eq!(m.readiness(),Readiness::Ready);
        assert_eq!(m.cards,Some(Vec::new()));
    }
    #[test]
    fn readiness_does_not_hide_successful_readings_during_service_changes() {
        let mut m = registered(8);
        m.modem_started = Some(false);
        m.incomplete = true;
        assert_eq!(m.readiness(),Readiness::Partial);
        m.checking = true;
        assert_eq!(m.readiness(),Readiness::Checking);
    }
}
