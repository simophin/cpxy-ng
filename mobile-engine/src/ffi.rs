//! The UniFFI surface the apps call, generating the Kotlin and Swift bindings (see
//! `src/bin/uniffi-bindgen.rs`). A thin wrapper over [`crate::start`].

use crate::{EngineHandle, EventListener, OutboundEvent, TrafficStats};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum EngineError {
    #[error("{message}")]
    Failed { message: String },
}

/// A TCP flow connected through an outbound, or failing to.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ConnectionEvent {
    pub host: String,
    pub port: u16,
    /// `direct` or `proxy`.
    pub outbound: String,
    /// How long connecting took.
    pub delay_millis: u64,
    /// When the flow was opened, in milliseconds since the Unix epoch.
    pub time_millis: u64,
    /// Set when connecting failed.
    pub error: Option<String>,
}

impl From<OutboundEvent> for ConnectionEvent {
    fn from(event: OutboundEvent) -> Self {
        match event {
            OutboundEvent::Connected {
                host,
                port,
                outbound,
                delay_mills,
                request_time_mills,
            } => Self {
                host,
                port,
                outbound: outbound.into_owned(),
                delay_millis: delay_mills as u64,
                time_millis: request_time_mills,
                error: None,
            },
            OutboundEvent::Error {
                host,
                port,
                outbound,
                delay_mills,
                request_time_mills,
                error,
            } => Self {
                host,
                port,
                outbound: outbound.into_owned(),
                delay_millis: delay_mills as u64,
                time_millis: request_time_mills,
                error: Some(error),
            },
        }
    }
}

/// Implemented by the app. Called from engine threads, so it must not block or call
/// [`Engine::stop`].
#[uniffi::export(with_foreign)]
pub trait EngineListener: Send + Sync {
    fn on_connection(&self, event: ConnectionEvent);
}

struct ListenerAdapter(Arc<dyn EngineListener>);

impl EventListener for ListenerAdapter {
    fn on_outbound_event(&self, event: OutboundEvent) {
        self.0.on_connection(event.into());
    }
}

#[derive(uniffi::Object)]
pub struct Engine(EngineHandle);

/// Starts the engine on `tun_fd`, which it takes ownership of and closes when stopped. Blocks
/// while the DNS servers are set up, so do not call it on a UI thread. See [`crate::start`].
#[uniffi::export]
pub fn start_engine(
    tun_fd: i32,
    config_json: String,
    listener: Arc<dyn EngineListener>,
) -> Result<Arc<Engine>, EngineError> {
    init_logging();
    if tun_fd < 0 {
        return Err(EngineError::Failed {
            message: format!("Invalid TUN descriptor {tun_fd}"),
        });
    }
    // SAFETY: the caller hands over a TUN descriptor it no longer uses.
    let tun = unsafe { OwnedFd::from_raw_fd(tun_fd) };
    crate::start(tun, &config_json, Arc::new(ListenerAdapter(listener)))
        .map(|handle| Arc::new(Engine(handle)))
        .map_err(|e| EngineError::Failed {
            message: format!("{e:#}"),
        })
}

#[uniffi::export]
impl Engine {
    /// Stops the engine and closes the TUN descriptor; blocks for up to a few seconds. Calling it
    /// again does nothing.
    pub fn stop(&self) {
        self.0.stop();
    }

    pub fn traffic(&self) -> TrafficStats {
        self.0.traffic()
    }
}

/// Sends the engine's logs to logcat on Android.
fn init_logging() {
    #[cfg(target_os = "android")]
    {
        use tracing_subscriber::layer::SubscriberExt;
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::INFO)
            .with(paranoid_android::layer("cpxy-engine"));
        // Fails when already set by an earlier start, which is fine.
        let _ = tracing::subscriber::set_global_default(subscriber);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    #[test]
    fn converts_outbound_events() {
        let event: ConnectionEvent = OutboundEvent::Error {
            host: "1.2.3.4".into(),
            port: 443,
            outbound: Cow::Borrowed("proxy"),
            delay_mills: 12,
            request_time_mills: 34,
            error: "refused".into(),
        }
        .into();
        assert_eq!(
            event,
            ConnectionEvent {
                host: "1.2.3.4".into(),
                port: 443,
                outbound: "proxy".into(),
                delay_millis: 12,
                time_millis: 34,
                error: Some("refused".into()),
            }
        );
    }

    #[test]
    fn rejects_a_negative_descriptor() {
        struct Ignore;
        impl EngineListener for Ignore {
            fn on_connection(&self, _: ConnectionEvent) {}
        }
        let config = r#"{"server": "http://:k@1.2.3.4", "dns_upstream": ["1.1.1.1"], "dns_alternative": ["8.8.8.8"]}"#;
        assert!(start_engine(-1, config.into(), Arc::new(Ignore)).is_err());
    }
}
