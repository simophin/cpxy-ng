//! The UniFFI surface the app calls (see `src/bin/uniffi-bindgen.rs`). A thin wrapper over
//! [`crate::start`].

use crate::profile::Profile;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
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

struct PlatformAdapter(Box<dyn RtpPlatform>);

impl crate::Platform for PlatformAdapter {
    fn protect_socket(&self, socket: &std::net::UdpSocket) -> std::io::Result<()> {
        if self.0.protect_socket(socket.as_raw_fd()) {
            Ok(())
        } else {
            Err(std::io::Error::other("Cannot protect the socket"))
        }
    }
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
    init_logging();
    if tun_fd < 0 {
        return Err(failed(format!("Invalid TUN descriptor {tun_fd}")));
    }
    // SAFETY: the caller hands over a TUN descriptor it no longer uses.
    let tun = unsafe { OwnedFd::from_raw_fd(tun_fd) };
    let key: [u8; 32] = key
        .try_into()
        .map_err(|_| failed("The key must be 32 bytes"))?;
    let profile = Profile::from_json(&profile_json).map_err(|e| failed(format!("{e:#}")))?;
    // Errors never carry the key: it is only handed to the worker.
    crate::start(tun, &profile, key, Arc::new(PlatformAdapter(platform)))
        .map(|engine| Arc::new(RtpEngine { engine }))
        .map_err(|e| failed(format!("{e:#}")))
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
    Profile::from_json(&profile_json)
        .and_then(|p| p.settings())
        .err()
        .map(|e| format!("{e:#}"))
}

/// Sends the engine's logs to logcat on Android.
fn init_logging() {
    #[cfg(target_os = "android")]
    {
        use tracing_subscriber::layer::SubscriberExt;
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::INFO)
            .with(paranoid_android::layer("cpxy-rtp").with_ansi(false));
        // Fails when already set by an earlier start, which is fine.
        let _ = tracing::subscriber::set_global_default(subscriber);
    }
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
