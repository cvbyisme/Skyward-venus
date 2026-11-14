//! One read-only NAS Get System Info request. No subscription, radio setter,
//! packet-data call, subscriber identifier, cell identifier, or raw TLV output.
//! Schema: libqmi 3454c2c619d516fd46a08fe16c6eebdee061857d,
//! data/qmi-service-nas.json (Get System Info, message 0x004D).

#[cfg(any(target_os = "android", test))]
#[path = "../src/qmi.rs"]
#[allow(dead_code)]
mod qmi;
#[cfg(any(target_os = "android", test))]
#[path = "../src/qrtr.rs"]
#[allow(dead_code)]
mod qrtr;

#[cfg(any(target_os = "android", test))]
mod safe_info {
    use super::{qmi, qrtr};
    use anyhow::{Result, ensure};
    use serde::Serialize;

    pub const MESSAGE: u16 = 0x004d;

    #[derive(Debug, PartialEq, Eq, Serialize)]
    pub struct Service {
        status: u8,
        true_status: u8,
        preferred_data_path: bool,
    }
    #[derive(Debug, PartialEq, Eq, Serialize)]
    pub struct Rejection {
        domain: u8,
        cause: u8,
    }
    #[derive(Debug, PartialEq, Eq, Serialize)]
    pub struct System {
        // None means the corresponding valid flag was zero, not value zero.
        domain: Option<u8>,
        forbidden: Option<bool>,
        rejection: Option<Rejection>,
    }
    #[derive(Debug, PartialEq, Eq, Serialize)]
    pub struct Radio {
        // None means the modem omitted this optional TLV altogether.
        service: Option<Service>,
        system: Option<System>,
    }
    #[derive(Debug, PartialEq, Eq, Serialize)]
    pub struct Info {
        lte: Radio,
        nr5g: Radio,
    }

    pub fn primary(endpoints: &[qrtr::Endpoint]) -> Result<qrtr::Endpoint> {
        let found: Vec<_> = endpoints
            .iter()
            .copied()
            .filter(|e| e.service == 3 && e.node == 0 && e.instance == 0)
            .collect();
        ensure!(
            found.len() == 1,
            "expected exactly one primary NAS endpoint, found {}",
            found.len()
        );
        Ok(found[0])
    }

    fn boolean(value: u8, field: &str) -> Result<bool> {
        ensure!(value <= 1, "invalid boolean in NAS {field}");
        Ok(value == 1)
    }
    fn service(bytes: &[u8]) -> Result<Service> {
        ensure!(
            bytes.len() == 3,
            "NAS service status must be exactly 3 bytes"
        );
        Ok(Service {
            status: bytes[0],
            true_status: bytes[1],
            preferred_data_path: boolean(bytes[2], "preferred data path")?,
        })
    }
    fn system(bytes: &[u8]) -> Result<System> {
        ensure!(
            bytes.len() == 29,
            "NAS LTE/NR system info must be exactly 29 bytes"
        );
        // Validate every validity flag in the official fixed-size layout, even
        // for private fields which are deliberately neither decoded nor output.
        for offset in [0, 2, 4, 6, 8, 11, 16, 19, 26] {
            boolean(bytes[offset], "system info validity flag")?;
        }
        let domain = (bytes[0] == 1).then_some(bytes[1]);
        let forbidden = if bytes[6] == 1 {
            Some(boolean(bytes[7], "forbidden")?)
        } else {
            None
        };
        let rejection = (bytes[16] == 1).then_some(Rejection {
            domain: bytes[17],
            cause: bytes[18],
        });
        // LAC/CID, MCC/MNC and TAC bytes are never copied into the result.
        Ok(System {
            domain,
            forbidden,
            rejection,
        })
    }
    pub fn decode(response: &qmi::Response<'_>) -> Result<Info> {
        let radio = |status, info| -> Result<Radio> {
            Ok(Radio {
                service: response.optional(status).map(service).transpose()?,
                system: response.optional(info).map(system).transpose()?,
            })
        };
        Ok(Info {
            lte: radio(0x14, 0x19)?,
            nr5g: radio(0x4a, 0x4b)?,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn packet(fields: &[(u8, &[u8])]) -> Vec<u8> {
            let mut bytes = qmi::request(7, MESSAGE, fields).unwrap();
            bytes[0] = 2;
            bytes
        }
        fn read(bytes: &[u8]) -> Result<Info> {
            decode(&qmi::Response::decode(bytes, 7, MESSAGE)?)
        }

        #[test]
        fn decodes_selected_values_without_identity_or_location() {
            let mut lte = [0u8; 29];
            lte[0] = 1;
            lte[1] = 2;
            lte[6] = 1;
            lte[7] = 1;
            lte[16] = 1;
            lte[17] = 2;
            lte[18] = 10;
            // Pretend private fields are present; they must never appear in JSON.
            lte[8] = 1;
            lte[9..11].copy_from_slice(&[0xab, 0xcd]);
            lte[11] = 1;
            lte[12..16].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
            lte[19] = 1;
            lte[20..26].copy_from_slice(b"123456");
            lte[26] = 1;
            lte[27..29].copy_from_slice(&[0xfe, 0xdc]);
            let bytes = packet(&[
                (2, &[0, 0, 0, 0]),
                (0x14, &[1, 2, 1]),
                (0x19, &lte),
                (0x4a, &[0, 0, 0]),
            ]);
            let info = read(&bytes).unwrap();
            assert_eq!(
                info.lte.system.as_ref().unwrap().rejection,
                Some(Rejection {
                    domain: 2,
                    cause: 10
                })
            );
            let json = serde_json::to_value(&info).unwrap();
            assert_eq!(
                json,
                serde_json::json!({
                    "lte": {"service":{"status":1,"true_status":2,"preferred_data_path":true},
                        "system":{"domain":2,"forbidden":true,"rejection":{"domain":2,"cause":10}}},
                    "nr5g":{"service":{"status":0,"true_status":0,"preferred_data_path":false},"system":null}
                })
            );
        }

        #[test]
        fn invalid_values_are_not_treated_as_valid_zero_and_missing_is_distinct() {
            let mut nr = [0xffu8; 29];
            for offset in [0, 2, 4, 6, 8, 11, 16, 19, 26] {
                nr[offset] = 0;
            }
            let info = read(&packet(&[(2, &[0, 0, 0, 0]), (0x4b, &nr)])).unwrap();
            assert_eq!(
                info.nr5g.system,
                Some(System {
                    domain: None,
                    forbidden: None,
                    rejection: None
                })
            );
            assert_eq!(info.lte.system, None);
            assert_eq!(info.nr5g.service, None);
        }

        #[test]
        fn rejects_truncation_extensions_bad_flags_and_bad_active_booleans() {
            for length in [0, 1, 2, 28, 30, 31] {
                assert!(read(&packet(&[(2, &[0, 0, 0, 0]), (0x19, &vec![0; length])])).is_err());
            }
            for length in [0, 1, 2, 4] {
                assert!(read(&packet(&[(2, &[0, 0, 0, 0]), (0x14, &vec![0; length])])).is_err());
            }
            for offset in [0, 2, 4, 6, 8, 11, 16, 19, 26] {
                let mut info = [0u8; 29];
                info[offset] = 2;
                assert!(read(&packet(&[(2, &[0, 0, 0, 0]), (0x4b, &info)])).is_err());
            }
            let mut info = [0u8; 29];
            info[6] = 1;
            info[7] = 2;
            assert!(read(&packet(&[(2, &[0, 0, 0, 0]), (0x19, &info)])).is_err());
            assert!(read(&packet(&[(2, &[0, 0, 0, 0]), (0x14, &[2, 2, 2])])).is_err());
        }

        #[test]
        fn failed_or_wrong_transaction_response_is_never_status() {
            assert!(read(&packet(&[(2, &[1, 0, 74, 0])])).is_err());
            let mut bytes = packet(&[(2, &[0, 0, 0, 0])]);
            bytes[1] = 8;
            assert!(read(&bytes).is_err());
            bytes[1] = 7;
            bytes[3] = 0x4e;
            assert!(read(&bytes).is_err());
            bytes[3] = 0x4d;
            bytes[0] = 4;
            assert!(read(&bytes).is_err());
            assert!(read(&packet(&[])).is_err());
        }

        #[test]
        fn only_one_exact_primary_nas_endpoint_is_accepted() {
            let primary_nas = qrtr::Endpoint {
                service: 3,
                instance: 0,
                version: 1,
                node: 0,
                port: 123,
            };
            let secondary = qrtr::Endpoint {
                instance: 1,
                ..primary_nas
            };
            let remote = qrtr::Endpoint {
                node: 1,
                ..primary_nas
            };
            let dms = qrtr::Endpoint {
                service: 2,
                ..primary_nas
            };
            assert_eq!(
                primary(&[secondary, remote, dms, primary_nas]).unwrap(),
                primary_nas
            );
            assert!(primary(&[secondary, remote, dms]).is_err());
            assert!(
                primary(&[
                    primary_nas,
                    qrtr::Endpoint {
                        port: 124,
                        ..primary_nas
                    }
                ])
                .is_err()
            );
        }
    }
}

#[cfg(target_os = "android")]
fn main() -> anyhow::Result<()> {
    use anyhow::ensure;
    ensure!(
        std::env::args_os().len() == 1,
        "usage: venus_nas_info (no arguments; read-only one-shot query)"
    );
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "requires root for the primary modem status socket"
    );
    let endpoint = safe_info::primary(&qrtr::discover()?)?;
    let info = qrtr::Client::new(endpoint)?.request(
        safe_info::MESSAGE,
        &[],
        std::time::Duration::from_secs(3),
        safe_info::decode,
    )?;
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) } == 0,
        "read boottime clock"
    );
    let boottime_ms = (time.tv_sec as u64) * 1000 + (time.tv_nsec as u64) / 1_000_000;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "read_only":true, "message":safe_info::MESSAGE, "endpoint":endpoint,
            "boottime_ms":boottime_ms, "system_info":info
        }))?
    );
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("venus_nas_info runs only on Android; host supports parser tests")
}
