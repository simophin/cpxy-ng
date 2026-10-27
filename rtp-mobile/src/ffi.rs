//! The UniFFI surface the app calls (see `src/bin/uniffi-bindgen.rs`). A thin wrapper over
//! the shared Linux packet engine. Android transport integration remains deferred.

use crate::profile::Profile;
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum RtpError {
    #[error("{reason}")]
    Failed { reason: String },
}

fn failed(reason: impl Into<String>) -> RtpError {
    RtpError::Failed {
        reason: reason.into(),
    }
}

#[uniffi::export(callback_interface)]
pub trait RtpPlatform: Send + Sync {
    /// Makes the socket bypass the VPN (`VpnService.protect`); the descriptor is borrowed.
    fn protect_socket(&self, fd: i32) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RtpState {
    Connecting,
    Connected,
    Repairing,
    Degraded,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RtpStatus {
    pub state: RtpState,
    pub error: Option<String>,
    /// IP packets and bytes from the device into the tunnel.
    pub sent_packets: u64,
    pub sent_bytes: u64,
    /// IP packets and bytes from the tunnel to the device.
    pub received_packets: u64,
    pub received_bytes: u64,
    pub dns_queries: u64,
    pub refused: u64,
    pub repairs: u64,
    pub fallbacks: u64,
}

impl From<crate::Status> for RtpStatus {
    fn from(s: crate::Status) -> Self {
        use crate::State;
        Self {
            state: match s.state {
                State::Connecting => RtpState::Connecting,
                State::Connected => RtpState::Connected,
                State::Repairing => RtpState::Repairing,
                State::Degraded => RtpState::Degraded,
                State::Failed => RtpState::Failed,
                State::Stopped => RtpState::Stopped,
            },
            error: s.error,
            sent_packets: s.sent_packets,
            sent_bytes: s.sent_bytes,
            received_packets: s.received_packets,
            received_bytes: s.received_bytes,
            dns_queries: s.dns_queries,
            refused: s.refused,
            repairs: s.repairs,
            fallbacks: s.fallbacks,
        }
    }
}

#[derive(uniffi::Object)]
pub struct RtpEngine {
    engine: crate::Engine,
}

/// Starts the engine on `tun_fd`, which it takes ownership of and closes when stopped. Blocks
/// while the endpoint is looked up and DNS is set up, so do not call it on a UI thread.
#[uniffi::export]
pub fn start_rtp_engine(
    tun_fd: i32,
    profile_json: String,
    key: Vec<u8>,
    platform: Box<dyn RtpPlatform>,
) -> Result<Arc<RtpEngine>, RtpError> {
    // Android migration is outside the Linux/OpenWrt scope. Preserve the public
    // binding and FD ownership, but never start a legacy RTP worker.
    let _ = (profile_json, key, platform);
    if tun_fd < 0 {
        return Err(failed(format!("Invalid TUN descriptor {tun_fd}")));
    }
    let _tun = unsafe { OwnedFd::from_raw_fd(tun_fd) };
    Err(failed(
        "HTTP-controlled RTP is available on Linux/OpenWrt; Android integration is deferred",
    ))
}

#[uniffi::export]
impl RtpEngine {
    /// Stops the engine and closes the TUN descriptor. Calling it again does nothing.
    pub fn stop(&self) {
        self.engine.stop();
    }

    pub fn network_changed(&self) {
        self.engine.network_changed();
    }

    pub fn status(&self) -> RtpStatus {
        self.engine.status().into()
    }
}

/// The IPv4 routes to add to the TUN, as `address/prefix`.
#[uniffi::export]
pub fn tunnel_routes() -> Vec<String> {
    crate::tunnel_routes()
        .iter()
        .map(|r| r.to_string())
        .collect()
}

/// The DNS server to give the TUN.
#[uniffi::export]
pub fn dns_address() -> String {
    crate::DNS_ADDRESS.to_string()
}

#[uniffi::export]
pub fn tunnel_mtu() -> u16 {
    crate::MTU
}

/// The 32-byte tunnel key for a password, as the endpoint derives it. Slow (Argon2id, 19 MiB):
/// call it off the UI thread.
#[uniffi::export]
pub fn derive_key(password: String) -> Result<Vec<u8>, RtpError> {
    rtp_l3::derive_psk(&password)
        .map(|key| key.to_vec())
        .map_err(failed)
}

/// Why a profile cannot be used, or `None` when it can.
#[uniffi::export]
pub fn validate_profile(profile_json: String) -> Option<String> {
    // The deferred app still emits its former manual-address field.
    let profile_json = serde_json::from_str::<serde_json::Value>(&profile_json)
        .map(|mut value| {
            if let Some(object) = value.as_object_mut() {
                object.remove("address");
            }
            value.to_string()
        })
        .unwrap_or(profile_json);
    Profile::from_json(&profile_json)
        .and_then(|p| p.settings())
        .err()
        .map(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_input_before_starting() {
        struct Never;
        impl RtpPlatform for Never {
            fn protect_socket(&self, _: i32) -> bool {
                false
            }
        }
        let profile = r#"{"endpoint":"192.0.2.1:30000","address":"10.88.0.2","dns_upstream":["223.5.5.5"],"dns_alternative":["8.8.8.8"]}"#;
        assert!(start_rtp_engine(-1, profile.into(), vec![0; 32], Box::new(Never)).is_err());
        assert!(validate_profile(profile.into()).is_none());
        assert!(validate_profile("{}".into()).is_some());
        assert!(derive_key("short".into()).is_err());
        assert_eq!(derive_key("long enough password".into()).unwrap().len(), 32);
    }
}
