//! The UniFFI surface the apps call, generating the Kotlin and Swift bindings (see
//! `src/bin/uniffi-bindgen.rs`). A thin wrapper over [`crate::start`].

use crate::{ConnectionPage, EngineHandle, TrafficStats};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum EngineError {
    #[error("{reason}")]
    Failed { reason: String },
}

#[derive(uniffi::Object)]
pub struct Engine {
    handle: EngineHandle,
}

/// Starts the engine on `tun_fd`, which it takes ownership of and closes when stopped. Blocks
/// while the DNS servers are set up, so do not call it on a UI thread. See [`crate::start`].
#[uniffi::export]
pub fn start_engine(tun_fd: i32, config_json: String) -> Result<Arc<Engine>, EngineError> {
    init_logging();
    if tun_fd < 0 {
        return Err(EngineError::Failed {
            reason: format!("Invalid TUN descriptor {tun_fd}"),
        });
    }
    // SAFETY: the caller hands over a TUN descriptor it no longer uses.
    let tun = unsafe { OwnedFd::from_raw_fd(tun_fd) };
    crate::start(tun, &config_json, Arc::new(()))
        .map(|handle| Arc::new(Engine { handle }))
        .map_err(|e| EngineError::Failed {
            reason: format!("{e:#}"),
        })
}

#[uniffi::export]
impl Engine {
    /// Stops the engine and closes the TUN descriptor; blocks for up to a few seconds. Calling it
    /// again does nothing.
    pub fn stop(&self) {
        self.handle.stop();
    }

    pub fn traffic(&self) -> TrafficStats {
        self.handle.traffic()
    }

    /// The loopback port of an HTTP proxy for the app's own requests, which bypass the VPN, to
    /// connect the way the VPN does.
    pub fn local_proxy_port(&self) -> Option<u16> {
        self.handle.local_proxy_port()
    }

    /// The oldest `limit` of the recent connections from `since` on. When `since` is before
    /// [`ConnectionPage::oldest_seq`], connections in between were dropped.
    pub fn connections_since(&self, since: u64, limit: u32) -> ConnectionPage {
        self.handle.connections_since(since, limit)
    }

    /// The newest `limit` of the recent connections before `before`, or the newest of all when
    /// it is `None`.
    pub fn connections_before(&self, before: Option<u64>, limit: u32) -> ConnectionPage {
        self.handle.connections_before(before, limit)
    }
}

/// Sends the engine's logs to logcat on Android.
fn init_logging() {
    #[cfg(target_os = "android")]
    {
        use tracing_subscriber::layer::SubscriberExt;
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::INFO)
            .with(paranoid_android::layer("cpxy-engine").with_ansi(false));
        // Fails when already set by an earlier start, which is fine.
        let _ = tracing::subscriber::set_global_default(subscriber);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_a_negative_descriptor() {
        let config = r#"{"server": "http://:k@1.2.3.4", "dns_upstream": ["1.1.1.1"], "dns_alternative": ["8.8.8.8"]}"#;
        assert!(start_engine(-1, config.into()).is_err());
    }
}

/// Platform policy/keystore adapter for the raw packet engine. All failures are
/// fail closed. `password` returns the shared password and must never log it;
/// an empty string means unavailable.
#[uniffi::export(callback_interface)]
pub trait PacketPlatform: Send + Sync {
    fn password(&self, endpoint_identity: String, reference: String) -> String;
    /// JSON PacketProfile. Return true only after persistent routes/guards and
    /// private DNS no-fallback policy have been applied successfully.
    fn prepare_network(&self, profile_json: String) -> bool;
    /// Borrowed descriptor; return true only after VPN bypass is effective.
    fn protect_socket(&self, fd: i32) -> bool;
}
struct PacketPlatformAdapter(Box<dyn PacketPlatform>);
impl crate::packet::Platform for PacketPlatformAdapter {
    fn resolve_password(&self, identity: &str, reference: &str) -> std::io::Result<String> {
        let password = self.0.password(identity.into(), reference.into());
        if password.is_empty() {
            return Err(std::io::Error::other("Password unavailable"));
        }
        Ok(password)
    }
    fn prepare_network(&self, profile: &crate::packet::PacketProfile) -> std::io::Result<()> {
        if self
            .0
            .prepare_network(serde_json::to_string(profile).unwrap())
        {
            Ok(())
        } else {
            Err(std::io::Error::other("Policy unavailable"))
        }
    }
    fn protect_socket(&self, socket: &std::net::UdpSocket) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        if self.0.protect_socket(socket.as_raw_fd()) {
            Ok(())
        } else {
            Err(std::io::Error::other("Protection failed"))
        }
    }
}
#[derive(uniffi::Object)]
pub struct PacketEngine {
    handle: crate::packet::PacketEngine,
}
#[uniffi::export]
pub fn start_packet_engine(
    tun_fd: i32,
    profile_json: String,
    platform: Box<dyn PacketPlatform>,
) -> Result<Arc<PacketEngine>, EngineError> {
    if tun_fd < 0 {
        return Err(EngineError::Failed {
            reason: "Invalid packet descriptor".into(),
        });
    }
    // SAFETY: as with start_engine, the caller transfers descriptor ownership.
    let tun = unsafe { OwnedFd::from_raw_fd(tun_fd) };
    let start = || -> anyhow::Result<_> {
        let profile =
            match serde_json::from_str::<crate::packet::ApplicationProfile>(&profile_json)? {
                crate::packet::ApplicationProfile::RtpL3(p) => p,
                _ => anyhow::bail!("RTP profile required"),
            };
        crate::packet::PacketEngine::start(tun, profile, Arc::new(PacketPlatformAdapter(platform)))
    };
    start()
        .map(|handle| Arc::new(PacketEngine { handle }))
        .map_err(|_| EngineError::Failed {
            reason:
                "RTP startup failed; check platform policy, password reference and socket protection"
                    .into(),
        })
}
#[uniffi::export]
impl PacketEngine {
    pub fn stop(&self) {
        self.handle.stop();
    }
    /// Contains status, numeric counters, effective routes and DNS policy;
    /// excludes password references, passwords and control payloads.
    pub fn diagnostics_json(&self) -> String {
        serde_json::to_string(&self.handle.diagnostics()).unwrap()
    }
}
