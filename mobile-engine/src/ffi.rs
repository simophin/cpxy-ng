//! The UniFFI surface the apps call, generating the Kotlin and Swift bindings (see
//! `src/bin/uniffi-bindgen.rs`). A thin wrapper over [`crate::start`].

use crate::connection_log::{self, ConnectionLog, ConnectionPage};
use crate::{EngineHandle, EventListener, OutboundEvent, TrafficStats};
use cpxy_ng::geoip::find_country_code_v4;
use geoip_data::GEOIP;
use std::net::Ipv4Addr;
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum EngineError {
    #[error("{reason}")]
    Failed { reason: String },
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
    /// The ISO 3166-1 alpha-2 code of the country `host` is in, when known.
    pub country_code: Option<String>,
}

fn country_code_of(host: &str) -> Option<String> {
    let ip: Ipv4Addr = host.parse().ok()?;
    find_country_code_v4(&ip, GEOIP)
        .ok()
        .flatten()
        .map(str::to_owned)
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
                country_code: country_code_of(&host),
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
                country_code: country_code_of(&host),
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

/// Records the engine's connections in the log. Called from an engine thread.
struct LogListener(Arc<ConnectionLog>);

impl EventListener for LogListener {
    fn on_outbound_event(&self, event: OutboundEvent) {
        self.0.push(event.into());
    }
}

#[derive(uniffi::Object)]
pub struct Engine {
    handle: EngineHandle,
    connections: Arc<ConnectionLog>,
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
    let connections = Arc::new(ConnectionLog::new(connection_log::CAPACITY));
    crate::start(
        tun,
        &config_json,
        Arc::new(LogListener(connections.clone())),
    )
    .map(|handle| {
        Arc::new(Engine {
            handle,
            connections,
        })
    })
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

    /// The oldest `limit` of the recent connections from `since` on. When `since` is before
    /// [`ConnectionPage::oldest_seq`], connections in between were dropped.
    pub fn connections_since(&self, since: u64, limit: u32) -> ConnectionPage {
        self.connections.since(since, limit as usize)
    }

    /// The newest `limit` of the recent connections before `before`, or the newest of all when
    /// it is `None`.
    pub fn connections_before(&self, before: Option<u64>, limit: u32) -> ConnectionPage {
        self.connections.before(before, limit as usize)
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
    use std::borrow::Cow;

    #[test]
    fn converts_outbound_events() {
        let event: ConnectionEvent = OutboundEvent::Error {
            host: "1.0.0.1".into(),
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
                host: "1.0.0.1".into(),
                port: 443,
                outbound: "proxy".into(),
                delay_millis: 12,
                time_millis: 34,
                error: Some("refused".into()),
                country_code: Some("AU".into()),
            }
        );
    }

    #[test]
    fn looks_up_country_codes() {
        assert_eq!(country_code_of("1.0.1.1").as_deref(), Some("CN"));
        assert_eq!(country_code_of("8.8.8.8").as_deref(), Some("US"));
        assert_eq!(country_code_of("192.168.1.1"), None);
        assert_eq!(country_code_of("example.com"), None);
    }

    #[test]
    fn rejects_a_negative_descriptor() {
        let config = r#"{"server": "http://:k@1.2.3.4", "dns_upstream": ["1.1.1.1"], "dns_alternative": ["8.8.8.8"]}"#;
        assert!(start_engine(-1, config.into()).is_err());
    }
}
