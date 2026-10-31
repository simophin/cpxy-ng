//! The UniFFI surface the apps call, generating the Kotlin and Swift bindings (see
//! `src/bin/uniffi-bindgen.rs`). A thin wrapper over [`crate::start`].

use crate::EngineHandle;
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
    crate::start(tun, &config_json)
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

    /// The loopback port of an HTTP proxy for the app's own requests, which bypass the VPN, to
    /// connect the way the VPN does.
    pub fn local_proxy_port(&self) -> Option<u16> {
        self.handle.local_proxy_port()
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

/// Compile before establishing the VPN; no sockets or TUN ownership are needed.
#[uniffi::export]
pub fn android_route_plan(
    proxy_routes: Vec<String>,
    ipv6: String,
    exclusions: bool,
    max_routes: u32,
) -> Result<crate::android_routes::AndroidRoutePlan, EngineError> {
    let compile = || -> anyhow::Result<_> {
        anyhow::ensure!(
            (4..=100_000).contains(&max_routes),
            "Route budget must be between 4 and 100000"
        );
        let routes = client::outbound::ProxyRoutes::parse(&proxy_routes)?;
        let mode: client::ipv6::Ipv6Mode = serde_json::from_value(serde_json::Value::String(ipv6))?;
        Ok(crate::android_routes::plan(
            &routes,
            mode,
            exclusions,
            max_routes as usize,
        ))
    };
    compile().map_err(|e| EngineError::Failed {
        reason: format!("{e:#}"),
    })
}
