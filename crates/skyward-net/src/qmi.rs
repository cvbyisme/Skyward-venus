//! QMI service packets over QRTR (no QMUX envelope). Decode only the
//! status fields needed by Recovery; never retain SIM identifiers or AIDs.
//! Wire schemas: libqmi data/qmi-service-{uim,nas,dms,wda}.json.
use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;

/// The modem's explicit protocol failure, distinct from transport or
/// decoding errors. Keep its numeric identity when adding caller context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolError { pub status: u16, pub code: u16 }

impl ProtocolError {
    /// Explicit startup unavailability from libqmi qmi-errors.h. These are
    /// distinct from InvalidTransition (60) and OperationInProgress (118).
    /// This predicate is only used by the bounded DMS startup transition.
    pub fn startup_unavailable(self) -> bool { self.status == 1 && matches!(self.code, 52|74) }
}
impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "QMI operation failed: status={} error={}", self.status, self.code)?;
        match self.code {
            52 => f.write_str(" (DeviceNotReady)")?,
            74 => f.write_str(" (InformationUnavailable)")?,
            _ => (),
        }
        Ok(())
    }
}
impl std::error::Error for ProtocolError {}

#[derive(Clone, Copy, Debug)]
pub enum StatusRequest { Cards, ServingSystem, HomeNetwork, Signal, OperatingMode, DataFormat }
impl StatusRequest {
    pub fn service(self) -> u32 { match self { Self::Cards => 11, Self::ServingSystem | Self::HomeNetwork | Self::Signal => 3, Self::OperatingMode => 2, Self::DataFormat => 26 } }
    pub fn message(self) -> u16 { match self { Self::Cards => 0x2f, Self::ServingSystem => 0x24, Self::HomeNetwork => 0x25, Self::Signal => 0x4f, Self::OperatingMode => 0x2d, Self::DataFormat => 0x21 } }
    pub fn encode(self, transaction: u16) -> Vec<u8> {
        let mut bytes = vec![0];
        bytes.extend(transaction.to_le_bytes());
        bytes.extend(self.message().to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        bytes
    }
}

/// A bounded QMI service request. Callers supply protocol fields, never a
/// QMUX envelope; transaction ownership stays with the QRTR client.
#[cfg(any(target_os = "android", test))]
pub(crate) fn request(transaction: u16, message: u16, fields: &[(u8, &[u8])]) -> Result<Vec<u8>> {
    let mut bytes = vec![0]; bytes.extend(transaction.to_le_bytes());
    bytes.extend(message.to_le_bytes()); bytes.extend([0,0]);
    let mut tags = std::collections::BTreeSet::new();
    for (tag,value) in fields {
        ensure!(tags.insert(*tag), "duplicate QMI request field");
        let size = u16::try_from(value.len()).context("QMI field too long")?;
        bytes.push(*tag); bytes.extend(size.to_le_bytes()); bytes.extend(*value);
    }
    let size = u16::try_from(bytes.len()-7).context("QMI request too long")?;
    bytes[5..7].copy_from_slice(&size.to_le_bytes()); Ok(bytes)
}

pub struct Response<'a> {
    fields: BTreeMap<u8, &'a [u8]>,
}
impl<'a> Response<'a> {
    pub fn decode(bytes: &'a [u8], transaction: u16, message: u16) -> Result<Self> {
        let response = Self::packet(bytes, 2, Some(transaction), message)?;
        if let Some(error) = response.failure()? {
            bail!(error);
        }
        Ok(response)
    }

    // Only WDS Start Network has these failure TLVs. Do not interpret
    // another service's tags as call diagnostics or accept failed replies.
    #[cfg(any(target_os = "android", test))]
    pub(crate) fn decode_wds_start(bytes: &'a [u8], transaction: u16) -> Result<Self> {
        let response = Self::packet(bytes, 2, Some(transaction), 0x20)?;
        if let Some(protocol) = response.failure()? {
            let error = anyhow::Error::new(protocol);
            return Err(match CallEndReasons::read(&response) {
                Ok(reason) => error.context(reason),
                Err(why) => {
                    error.context(format!("WDS Start failure diagnostics malformed: {why:#}"))
                }
            });
        }
        Ok(response)
    }

    fn packet(bytes: &'a [u8], flags: u8, transaction: Option<u16>, message: u16) -> Result<Self> {
        let mut reader = Cursor(bytes);
        ensure!(reader.byte()? == flags, "unexpected QMI packet type");
        let received = reader.word()?;
        ensure!(
            transaction.is_none_or(|expected| expected == received) && reader.word()? == message,
            "QMI packet belongs to another request or message"
        );
        let length = reader.word()? as usize;
        ensure!(reader.0.len() == length, "QMI payload length differs");
        let mut fields = BTreeMap::new();
        while !reader.0.is_empty() {
            let tag = reader.byte()?;
            let length = reader.word()? as usize;
            ensure!(
                fields.insert(tag, reader.take(length)?).is_none(),
                "duplicate QMI field {tag:#x}"
            );
        }
        Ok(Self { fields })
    }

    fn failure(&self) -> Result<Option<ProtocolError>> {
        let mut result = Cursor(self.required(2)?);
        let (status, error) = (result.word()?, result.word()?);
        ensure!(result.0.is_empty(), "invalid QMI result length");
        Ok((status != 0 || error != 0).then_some(ProtocolError {
            status,
            code: error,
        }))
    }
    pub fn required(&self, tag: u8) -> Result<&'a [u8]> {
        self.fields
            .get(&tag)
            .copied()
            .with_context(|| format!("QMI field {tag:#x} missing"))
    }
    pub fn optional(&self, tag: u8) -> Option<&'a [u8]> {
        self.fields.get(&tag).copied()
    }
}

/// Numeric WDS diagnostic fields only: no APN, profile credentials or IDs.
/// The verbose reason is signed on the wire, unlike its unsigned type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct VerboseCallEndReason {
    pub kind: u16,
    pub reason: i16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CallEndReasons {
    pub call_end_reason: Option<u16>,
    pub verbose: Option<VerboseCallEndReason>,
}
impl CallEndReasons {
    fn read(fields: &Response<'_>) -> Result<Self> {
        let call_end_reason = fields
            .optional(0x10)
            .map(|bytes| -> Result<u16> {
                Ok(u16::from_le_bytes(
                    bytes
                        .try_into()
                        .context("invalid WDS call-end reason length")?,
                ))
            })
            .transpose()?;
        let verbose = fields
            .optional(0x11)
            .map(|bytes| -> Result<VerboseCallEndReason> {
                ensure!(
                    bytes.len() == 4,
                    "invalid WDS verbose call-end reason length"
                );
                Ok(VerboseCallEndReason {
                    kind: u16::from_le_bytes([bytes[0], bytes[1]]),
                    reason: i16::from_le_bytes([bytes[2], bytes[3]]),
                })
            })
            .transpose()?;
        Ok(Self {
            call_end_reason,
            verbose,
        })
    }
}
impl std::fmt::Display for CallEndReasons {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "WDS call end: reason={:?} verbose_type={:?} verbose_reason={:?}",
            self.call_end_reason,
            self.verbose.map(|r| r.kind),
            self.verbose.map(|r| r.reason)
        )
    }
}

/// WDS 0x22 indication, distinct from the one-byte Get Status response.
/// Source endpoint/client validation belongs to the owning QRTR socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PacketServiceIndication {
    pub state: u8,
    pub reconfiguration_required: bool,
    pub ip_family: Option<u8>,
    pub reasons: CallEndReasons,
}
impl PacketServiceIndication {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let fields = Response::packet(bytes, 4, None, 0x22)?;
        ensure!(
            fields.optional(2).is_none(),
            "WDS indication contains a response result"
        );
        let state = fields.required(1)?;
        ensure!(
            state.len() == 2 && state[1] <= 1,
            "invalid WDS indication status/reconfiguration"
        );
        let ip_family = fields
            .optional(0x12)
            .map(|bytes| -> Result<u8> {
                ensure!(bytes.len() == 1, "invalid WDS indication IP family length");
                Ok(bytes[0])
            })
            .transpose()?;
        Ok(Self {
            state: state[0],
            reconfiguration_required: state[1] != 0,
            ip_family,
            reasons: CallEndReasons::read(&fields)?,
        })
    }
}

struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8]> {
        ensure!(self.0.len() >= size, "truncated QMI field");
        let (value, rest) = self.0.split_at(size); self.0 = rest; Ok(value)
    }
    fn byte(&mut self) -> Result<u8> { Ok(self.take(1)?[0]) }
    fn word(&mut self) -> Result<u16> { Ok(u16::from_le_bytes(self.take(2)?.try_into()?)) }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Card {
    /// Physical slot numbers shown to the user start at one.
    pub slot: u8,
    /// UIM: 0 absent, 1 present, 2 error; preserve unknown states.
    pub state: u8,
    pub error: u8,
    pub universal_pin_state: u8,
    pub applications: Vec<Application>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Application {
    pub kind: u8,
    pub state: u8,
    pub personalization_state: u8,
    pub pin1_state: u8,
    pub universal_pin_replaces_pin1: bool,
}

pub fn cards(response: &Response<'_>) -> Result<Vec<Card>> {
    let mut reader = Cursor(response.required(0x10)?);
    reader.take(8)?; // Provisioning application indexes, not subscriber IDs.
    let count = reader.byte()?;
    ensure!(count <= 8, "too many physical SIM slots");
    let mut cards = Vec::new();
    for slot in 1..=count {
        let state = reader.byte()?;
        let universal_pin_state = reader.byte()?;
        reader.take(2)?; // Retry counts; no credential operation is performed.
        let error = reader.byte()?;
        let count = reader.byte()?;
        ensure!(count <= 32, "too many SIM applications");
        let mut applications = Vec::new();
        for _ in 0..count {
            let (kind, state, personalization_state) = (reader.byte()?, reader.byte()?, reader.byte()?);
            reader.take(3)?; // Personalization feature and retry counters.
            let aid_length = reader.byte()? as usize;
            reader.take(aid_length)?; // Discard AID: never stored or logged.
            let universal_pin_replaces_pin1 = reader.byte()? != 0;
            let pin1_state = reader.byte()?;
            reader.take(5)?; // PIN/PUK retry counters and PIN2 state.
            applications.push(Application { kind, state, personalization_state, pin1_state, universal_pin_replaces_pin1 });
        }
        cards.push(Card { slot, state, error, universal_pin_state, applications });
    }
    ensure!(reader.0.is_empty(), "unexpected bytes in UIM card status");
    Ok(cards)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServingSystem {
    pub registration_state: u8,
    pub circuit_attach_state: u8,
    pub packet_attach_state: u8,
    pub selected_network: u8,
    pub radio_interfaces: Vec<u8>,
    pub plmn: Option<Plmn>,
    pub roaming: Option<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plmn {
    pub mcc: u16,
    pub mnc: u16,
    pub three_digit_mnc: Option<bool>,
    pub description: String,
}
fn plmn(bytes: &[u8]) -> Result<Plmn> {
    let mut reader = Cursor(bytes);
    let (mcc,mnc) = (reader.word()?,reader.word()?);
    ensure!(mcc <= 999 && mnc <= 999, "invalid PLMN code");
    let length = reader.byte()? as usize;
    // This legacy field has no encoding tag. Some Venus NAS reports put
    // UCS2 bytes here, which are not a UTF-8 string. Do not turn them into
    // printable garbage by lossy conversion/stripping control bytes; the
    // caller can ask the same PLMN's explicitly separate home name.
    let description = std::str::from_utf8(reader.take(length)?).ok()
        .filter(|s| !s.chars().any(char::is_control)).unwrap_or_default().to_string();
    ensure!(reader.0.is_empty(), "unexpected PLMN bytes");
    Ok(Plmn { mcc,mnc,three_digit_mnc:None,description })
}
pub fn home_network(response: &Response<'_>) -> Result<Plmn> { plmn(response.required(1)?) }
pub fn serving_system(response: &Response<'_>) -> Result<ServingSystem> {
    let mut reader = Cursor(response.required(1)?);
    let (registration_state, circuit_attach_state, packet_attach_state, selected_network) =
        (reader.byte()?, reader.byte()?, reader.byte()?, reader.byte()?);
    let count = reader.byte()? as usize;
    ensure!(count <= 32, "too many radio interfaces");
    let radio_interfaces = reader.take(count)?.to_vec();
    ensure!(reader.0.is_empty(), "unexpected bytes in NAS serving system");
    let plmn = response.optional(0x12).map(|bytes| -> Result<Plmn> {
        let mut network=plmn(bytes)?;
        let three_digit_mnc = response.optional(0x27).map(|bytes| -> Result<Option<bool>> {
            let mut reader = Cursor(bytes);
            let (other_mcc,other_mnc,digits) = (reader.word()?,reader.word()?,reader.byte()?);
            ensure!(reader.0.is_empty() && digits <= 1, "invalid PLMN digit status");
            Ok((other_mcc == network.mcc && other_mnc == network.mnc).then_some(digits != 0))
        }).transpose()?.flatten();
        network.three_digit_mnc=three_digit_mnc;
        Ok(network)
    }).transpose()?;
    let roaming = response.optional(0x10).map(|bytes| -> Result<u8> {
        ensure!(bytes.len() == 1, "invalid roaming indicator"); Ok(bytes[0])
    }).transpose()?;
    Ok(ServingSystem { registration_state, circuit_attach_state, packet_attach_state, selected_network, radio_interfaces,plmn,roaming })
}

/// NAS measurements are kept per radio. The UI must select the registered
/// radio, rather than substitute a stronger signal from another technology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Signal {
    pub radio: u8,
    pub rssi_dbm: Option<i16>,
    /// RSRP for LTE/NR; RSCP for WCDMA when reported.
    pub reference_dbm: Option<i16>,
}
pub fn signal(response: &Response<'_>) -> Result<Vec<Signal>> {
    let mut values = Vec::new();
    for (tag,radio,length) in [(0x10,1,3),(0x11,2,8),(0x12,4,1),(0x13,5,3),(0x14,8,6),(0x15,9,1),(0x17,12,4)] {
        let Some(bytes) = response.optional(tag) else { continue };
        ensure!(bytes.len() == length, "invalid NAS signal length for radio {radio}");
        let reference_dbm = match radio {
            8 => Some(i16::from_le_bytes(bytes[2..4].try_into()?)),
            12 => Some(i16::from_le_bytes(bytes[..2].try_into()?)),
            _ => None,
        };
        let rssi_dbm = (radio != 12).then(|| i16::from(bytes[0] as i8));
        values.push(Signal { radio,rssi_dbm,reference_dbm });
    }
    if let Some(bytes) = response.optional(0x19) {
        let rscp = i16::from_le_bytes(bytes.try_into().context("invalid WCDMA RSCP length")?);
        if let Some(wcdma) = values.iter_mut().find(|v| v.radio == 5) { wcdma.reference_dbm = Some(rscp); }
        else { values.push(Signal { radio: 5,rssi_dbm: None,reference_dbm: Some(rscp) }); }
    }
    Ok(values)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatingMode {
    /// DMS: 0 online, 1 low power, 2 factory test, 3 offline, 4 reset,
    /// 5 shutdown, 6 persistent low power, 7 mode-only low power.
    pub mode: u8,
    pub hardware_restricted: Option<bool>,
}
pub fn operating_mode(response: &Response<'_>) -> Result<OperatingMode> {
    let byte = |bytes: &[u8]| -> Result<u8> {
        ensure!(bytes.len() == 1, "invalid DMS scalar length"); Ok(bytes[0])
    };
    Ok(OperatingMode {
        mode: byte(response.required(1)?)?,
        hardware_restricted: response.optional(0x11).map(|v| byte(v).map(|b| b != 0)).transpose()?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataFormat {
    pub link_layer: u32,
    pub uplink_aggregation: Option<u32>,
    pub downlink_aggregation: Option<u32>,
    pub downlink_max_datagrams: Option<u32>,
    pub downlink_max_size: Option<u32>,
}
pub fn data_format(response: &Response<'_>) -> Result<DataFormat> {
    let dword = |bytes: &[u8]| -> Result<u32> { Ok(u32::from_le_bytes(bytes.try_into().context("invalid WDA scalar length")?)) };
    let optional = |tag| response.optional(tag).map(dword).transpose();
    Ok(DataFormat {
        link_layer: dword(response.required(0x11)?)?,
        uplink_aggregation: optional(0x12)?, downlink_aggregation: optional(0x13)?,
        downlink_max_datagrams: optional(0x15)?, downlink_max_size: optional(0x16)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wds_packet(flags: u8, message: u16, fields: &[(u8,&[u8])]) -> Vec<u8> {
        let mut bytes = vec![flags,9,0]; bytes.extend(message.to_le_bytes()); bytes.extend([0,0]);
        for (tag,value) in fields {
            bytes.push(*tag); bytes.extend((value.len() as u16).to_le_bytes()); bytes.extend(*value);
        }
        let size = (bytes.len()-7) as u16; bytes[5..7].copy_from_slice(&size.to_le_bytes()); bytes
    }

    #[test]
    fn wds_indications_preserve_numeric_reasons_without_becoming_responses() {
        let raw = wds_packet(4,0x22,&[(1,&[1,1]),(0x10,&[0x34,0x12]),(0x11,&[2,0,0xff,0xff]),(0x12,&[4])]);
        let indication = PacketServiceIndication::decode(&raw).unwrap();
        assert_eq!((indication.state,indication.reconfiguration_required,indication.ip_family),(1,true,Some(4)));
        assert_eq!(indication.reasons.call_end_reason,Some(0x1234));
        assert_eq!(indication.reasons.verbose,Some(VerboseCallEndReason {kind:2,reason:-1}));
        assert!(Response::decode(&raw,9,0x22).is_err());
        // Unknown future state/reason codes are retained, never reclassified.
        let unknown = wds_packet(4,0x22,&[(1,&[99,0]),(0x11,&[0xff,0xff,0xff,0x7f])]);
        assert_eq!(PacketServiceIndication::decode(&unknown).unwrap().state,99);
        assert_eq!(PacketServiceIndication::decode(&wds_packet(4,0x22,&[(1,&[2,0])])).unwrap().reasons,CallEndReasons::default());
    }

    #[test]
    fn wds_indication_rejects_foreign_or_malformed_wire_formats() {
        for fields in [vec![(1,&[1][..])],vec![(1,&[1,2][..])],vec![(1,&[1,0][..]),(1,&[2,0][..])],
            vec![(1,&[1,0][..]),(2,&[0,0,0,0][..])],vec![(1,&[1,0][..]),(0x10,&[1][..])],
            vec![(1,&[1,0][..]),(0x11,&[1,2,3][..])],vec![(1,&[1,0][..]),(0x12,&[4,0][..])]] {
            assert!(PacketServiceIndication::decode(&wds_packet(4,0x22,&fields)).is_err());
        }
        for (flags,message) in [(2,0x22),(4,0x20)] {
            assert!(PacketServiceIndication::decode(&wds_packet(flags,message,&[(1,&[1,0])])).is_err());
        }
        let raw = wds_packet(4,0x22,&[(1,&[1,0])]);
        for length in 0..raw.len() { assert!(PacketServiceIndication::decode(&raw[..length]).is_err()); }
        let mut trailing=raw;trailing.push(0);assert!(PacketServiceIndication::decode(&trailing).is_err());
    }

    #[test]
    fn wds_start_failure_keeps_protocol_identity_and_optional_numeric_reasons() {
        let raw = wds_packet(2,0x20,&[(2,&[1,0,14,0]),(0x10,&[2,0]),(0x11,&[3,0,4,0])]);
        let failure = match Response::decode_wds_start(&raw,9) { Err(e)=>e,Ok(_)=>panic!("failed start accepted") };
        assert_eq!(failure.downcast_ref::<ProtocolError>(),Some(&ProtocolError {status:1,code:14}));
        assert_eq!(failure.downcast_ref::<CallEndReasons>().unwrap().verbose,Some(VerboseCallEndReason {kind:3,reason:4}));
        // A malformed diagnostic must not erase the original QMI failure.
        let bad = wds_packet(2,0x20,&[(2,&[1,0,14,0]),(0x10,&[2])]);
        let failure = match Response::decode_wds_start(&bad,9) { Err(e)=>e,Ok(_)=>panic!("failed start accepted") };
        assert_eq!(failure.downcast_ref::<ProtocolError>().unwrap().code,14);
        assert!(failure.to_string().contains("malformed"));
        assert!(Response::decode_wds_start(&raw,8).is_err());
        let success = wds_packet(2,0x20,&[(2,&[0,0,0,0]),(1,&[1,2,3,4])]);
        assert_eq!(Response::decode_wds_start(&success,9).unwrap().required(1).unwrap(),[1,2,3,4]);
        // The ordinary decoder continues to expose only its protocol error.
        let generic = match Response::decode(&raw,9,0x20) { Err(e)=>e,Ok(_)=>panic!("failed result accepted") };
        assert!(generic.downcast_ref::<CallEndReasons>().is_none());
    }

    #[test]
    fn protocol_error_identity_survives_context_without_text_matching() {
        for (status, code, retryable) in [(1u16,74u16,true),(0,74,false),(2,74,false),
            (1,52,true),(0,52,false),(2,52,false),(1,60,false),(1,83,false),(1,118,false)] {
            let mut raw = vec![2,9,0,0x2f,0,7,0,2,4,0];
            raw.extend(status.to_le_bytes()); raw.extend(code.to_le_bytes());
            let error = match Response::decode(&raw,9,0x2f) {
                Err(e) => e.context("DMS startup"), Ok(_) => panic!("failed result was accepted"),
            };
            let protocol = error.downcast_ref::<ProtocolError>().unwrap();
            assert_eq!((protocol.status,protocol.code),(status,code));
            assert_eq!(protocol.startup_unavailable(),retryable);
        }
    }

    fn reply(fields: &[(u8, &[u8])]) -> Vec<u8> {
        let mut bytes = vec![2, 9, 0, 0x2f, 0, 0, 0];
        for (tag, value) in std::iter::once(&(2, &[0, 0, 0, 0][..])).chain(fields) {
            bytes.push(*tag); bytes.extend((value.len() as u16).to_le_bytes()); bytes.extend(*value);
        }
        let size = (bytes.len()-7) as u16; bytes[5..7].copy_from_slice(&size.to_le_bytes()); bytes
    }
    fn card_field() -> Vec<u8> {
        let mut card = vec![0,0,255,255,255,255,255,255,2];
        card.extend([1,3,3,10,0,1]);
        card.extend([2,7,2,0,0,0,4,0xaa,0xbb,0xcc,0xdd,0,3,3,10,3,3,10]);
        card.extend([0,0,0,0,0,0]); card
    }
    #[test]
    fn two_slots_and_variable_aid_are_decoded_without_retaining_identity() {
        let raw = reply(&[(0x10, &card_field())]);
        let response = Response::decode(&raw, 9, 0x2f).unwrap();
        let cards = cards(&response).unwrap();
        assert_eq!((cards[0].slot,cards[0].state,cards[0].applications[0].state), (1,1,7));
        assert_eq!((cards[1].slot,cards[1].state), (2,0));
        assert!(!serde_json::to_string(&cards).unwrap().contains("aabbccdd"));
    }
    #[test]
    fn every_truncation_is_refused_without_panic() {
        let raw = reply(&[(0x10, &card_field())]);
        for cut in 0..raw.len() { assert!(Response::decode(&raw[..cut],9,0x2f).is_err()); }
        let field = card_field();
        for cut in 0..field.len() {
            let raw = reply(&[(0x10,&field[..cut])]);
            assert!(cards(&Response::decode(&raw,9,0x2f).unwrap()).is_err());
        }
    }
    #[test]
    fn duplicate_fields_and_unrelated_responses_are_refused() {
        let raw = reply(&[(0x10,&card_field()),(0x10,&card_field())]);
        assert!(Response::decode(&raw,9,0x2f).is_err());
        let raw = reply(&[]);
        assert!(Response::decode(&raw,10,0x2f).is_err());
        assert!(Response::decode(&raw,9,0x24).is_err());
    }
    #[test]
    fn modem_error_is_not_reported_as_an_empty_card() {
        let mut raw = reply(&[]); raw[10..14].copy_from_slice(&[1,0,3,0]);
        assert!(format!("{:#}",Response::decode(&raw,9,0x2f).err().unwrap()).contains("error=3"));
    }
    #[test]
    fn nas_registration_and_nr_radio_are_preserved_and_truncation_is_refused() {
        let mut raw = reply(&[(1,&[1,2,1,0,1,12])]);
        raw[3..5].copy_from_slice(&0x24u16.to_le_bytes());
        let serving = serving_system(&Response::decode(&raw,9,0x24).unwrap()).unwrap();
        assert_eq!((serving.registration_state,serving.packet_attach_state), (1,1));
        assert_eq!(serving.radio_interfaces, [12]);
        let mut raw = reply(&[(1,&[1,2,1,0,2,12])]);
        raw[3..5].copy_from_slice(&0x24u16.to_le_bytes());
        assert!(serving_system(&Response::decode(&raw,9,0x24).unwrap()).is_err());
    }
    #[test]
    fn plmn_digits_and_roaming_are_decoded_without_subscriber_identifiers() {
        let mut raw = reply(&[(1,&[1,2,1,0,1,12]),(0x10,&[1]),
            (0x12,&[0xcc,1,15,0,3,b'C',b'B',b'N']),(0x27,&[0xcc,1,15,0,1])]);
        raw[3..5].copy_from_slice(&0x24u16.to_le_bytes());
        let serving=serving_system(&Response::decode(&raw,9,0x24).unwrap()).unwrap();
        assert_eq!(serving.roaming,Some(1));
        assert_eq!(serving.plmn,Some(Plmn { mcc:460,mnc:15,three_digit_mnc:Some(true),description:"CBN".into() }));
    }
    #[test]
    fn truncated_operator_text_is_not_shown_as_a_valid_network() {
        let field=[0xcc,1,15,0,3,b'C',b'B',b'N'];
        for cut in 0..field.len() {
            let mut raw=reply(&[(1,&[1,2,1,0,1,8]),(0x12,&field[..cut])]);
            raw[3..5].copy_from_slice(&0x24u16.to_le_bytes());
            assert!(serving_system(&Response::decode(&raw,9,0x24).unwrap()).is_err());
        }
    }
    #[test]
    fn home_operator_uses_the_home_network_field_and_keeps_unknown_mnc_width() {
        let mut raw=reply(&[(1,&[0xcc,1,15,0,3,b'C',b'B',b'N'])]);
        raw[3..5].copy_from_slice(&0x25u16.to_le_bytes());
        let home=home_network(&Response::decode(&raw,9,0x25).unwrap()).unwrap();
        assert_eq!((home.mcc,home.mnc,home.description.as_str()),(460,15,"CBN"));
        assert_eq!(home.three_digit_mnc,None);
        assert_eq!(StatusRequest::HomeNetwork.service(),3);
    }
    #[test]
    fn untagged_ucs2_operator_is_not_lossily_rendered_as_ascii_garbage() {
        let mut raw=reply(&[(1,&[1,2,1,0,1,8]),(0x12,&[0xcc,1,15,0,8,0x2d,0x4e,0xfd,0x56,0x7f,0x5e,0x35,0x75])]);
        raw[3..5].copy_from_slice(&0x24u16.to_le_bytes());
        let current=serving_system(&Response::decode(&raw,9,0x24).unwrap()).unwrap();
        assert_eq!(current.registration_state,1);
        assert_eq!(current.plmn.unwrap().description,"");
    }
    #[test]
    fn radio_and_data_format_keep_unknown_values_and_reject_bad_scalar_sizes() {
        let raw = reply(&[(1,&[6]),(0x11,&[1])]);
        let mode = operating_mode(&Response::decode(&raw,9,0x2f).unwrap()).unwrap();
        assert_eq!(mode,OperatingMode { mode: 6, hardware_restricted: Some(true) });
        let raw = reply(&[(1,&[])]);
        assert!(operating_mode(&Response::decode(&raw,9,0x2f).unwrap()).is_err());
        let raw = reply(&[(0x11,&2u32.to_le_bytes()),(0x12,&8u32.to_le_bytes())]);
        let format = data_format(&Response::decode(&raw,9,0x2f).unwrap()).unwrap();
        assert_eq!((format.link_layer,format.uplink_aggregation,format.downlink_aggregation),(2,Some(8),None));
        let raw = reply(&[(0x11,&[2,0,0])]);
        assert!(data_format(&Response::decode(&raw,9,0x2f).unwrap()).is_err());
    }
    #[test]
    fn request_sizes_and_unique_tags_are_checked() {
        assert_eq!(request(3,0x2e,&[(1,&[0])]).unwrap(),[0,3,0,0x2e,0,4,0,1,1,0,0]);
        assert!(request(3,0x2e,&[(1,&[0]),(1,&[1])]).is_err());
        assert!(request(1,0,&[(1,&vec![0;65535])]).is_err());
    }
    #[test]
    fn lte_and_nr_signals_remain_distinct_and_signed() {
        let raw = reply(&[(0x14,&[196,246,166,255,200,0]),(0x17,&[151,255,122,0])]);
        let values = signal(&Response::decode(&raw,9,0x2f).unwrap()).unwrap();
        assert_eq!(values,[Signal { radio: 8,rssi_dbm: Some(-60),reference_dbm: Some(-90) },
            Signal { radio: 12,rssi_dbm: None,reference_dbm: Some(-105) }]);
        let raw = reply(&[]);
        assert!(signal(&Response::decode(&raw,9,0x2f).unwrap()).unwrap().is_empty());
    }
    #[test]
    fn truncated_signal_is_not_a_zero_or_full_strength_measurement() {
        for size in 0..6 {
            let raw = reply(&[(0x14,&[0;6][..size])]);
            assert!(signal(&Response::decode(&raw,9,0x2f).unwrap()).is_err());
        }
        let raw = reply(&[(0x19,&[156,255])]);
        assert_eq!(signal(&Response::decode(&raw,9,0x2f).unwrap()).unwrap()[0],
            Signal { radio: 5,rssi_dbm: None,reference_dbm: Some(-100) });
    }
}
