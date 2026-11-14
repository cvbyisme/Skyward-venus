pub mod dhcp;
pub mod dns;
pub mod download;
pub mod github;
pub mod http;
#[cfg(any(target_os = "android", test))]
pub mod iface;
pub mod reach;
pub mod release;
pub mod supplicant;
pub mod wlan;
pub mod qmi;
pub mod mobile;
pub mod cellular_data;
#[cfg(target_os = "android")]
pub mod qrtr;
#[cfg(target_os = "android")]
pub mod cellular;

/// Work stopped because it was asked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stopped as asked")
    }
}

impl std::error::Error for Stopped {}

pub mod modem;
