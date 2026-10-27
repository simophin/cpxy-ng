//! The UniFFI surface the app calls (see `src/bin/uniffi-bindgen.rs`). A thin wrapper over
//! the shared packet engine. Preparation owns the authenticated path until TUN handoff.

use crate::profile::Profile;
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum RtpError {
    #[error("Invalid profile. Check the HTTP endpoint, server identity and DNS settings.")]
    Configuration,
    #[error("Invalid saved client identity. Restore it or recreate the profile.")]
    Identity,
    #[error("Endpoint rejected control authentication. Check password and server identity.")]
    Rejected,
    #[error("Cannot reach the endpoint. Retrying when connectivity returns.")]
    Connectivity,
    #[error("Connection cancelled")]
    Cancelled,
    #[error("Preparation has already been run or consumed")]
    Consumed,
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

struct PlatformAdapter(Box<dyn RtpPlatform>);
impl crate::Platform for PlatformAdapter {
    fn protect_socket(&self, fd: i32) -> std::io::Result<()> {
        if self.0.protect_socket(fd) {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "socket protection failed",
            ))
        }
    }
}

struct Inputs {
    profile: Profile,
    key: zeroize::Zeroizing<[u8; 32]>,
    client_id: uuid::Uuid,
    platform: Arc<dyn crate::Platform>,
}

/// Create on a worker thread; `cancel` may be called concurrently from the UI thread.
/// `prepare`, `start` and destruction/cleanup must run off the UI thread.
#[derive(uniffi::Object)]
pub struct RtpPreparation {
    inputs: Mutex<Option<Inputs>>,
    prepared: Mutex<Option<crate::Prepared>>,
    cancellation: Arc<crate::PreparationCancellation>,
    used: AtomicBool,
    transferred: AtomicBool,
}

#[uniffi::export]
pub fn create_rtp_preparation(
    profile_json: String,
    mut key: Vec<u8>,
    client_id: String,
    platform: Box<dyn RtpPlatform>,
) -> Result<Arc<RtpPreparation>, RtpError> {
    init_logging();
    let checked_key = key.as_slice().try_into().map(zeroize::Zeroizing::new);
    use zeroize::Zeroize;
    key.zeroize();
    let key = checked_key.map_err(|_| RtpError::Configuration)?;
    let client_id = uuid::Uuid::parse_str(&client_id)
        .ok()
        .filter(|id| !id.is_nil() && id.to_string() == client_id)
        .ok_or(RtpError::Identity)?;
    let profile = Profile::from_json(&profile_json).map_err(|_| RtpError::Configuration)?;
    profile.settings().map_err(|_| RtpError::Configuration)?;
    Ok(Arc::new(RtpPreparation {
        inputs: Mutex::new(Some(Inputs {
            profile,
            key,
            client_id,
            platform: Arc::new(PlatformAdapter(platform)),
        })),
        prepared: Mutex::new(None),
        cancellation: Arc::default(),
        used: AtomicBool::new(false),
        transferred: AtomicBool::new(false),
    }))
}

#[uniffi::export]
impl RtpPreparation {
    /// Returns the assigned IPv4 only after authenticated UDP readiness; does not activate.
    pub fn prepare(&self) -> Result<String, RtpError> {
        if self.used.swap(true, Ordering::AcqRel) {
            return Err(RtpError::Consumed);
        }
        self.cancellation.check().map_err(|_| RtpError::Cancelled)?;
        let inputs = self
            .inputs
            .lock()
            .unwrap()
            .take()
            .ok_or(RtpError::Consumed)?;
        let result = crate::prepare_cancellable(
            &inputs.profile,
            *inputs.key,
            inputs.client_id,
            inputs.platform,
            self.cancellation.clone(),
        );
        self.cancellation.check().map_err(|_| RtpError::Cancelled)?;
        let path = result.map_err(preparation_error)?;
        let address = path.address().to_string();
        let mut prepared = self.prepared.lock().unwrap();
        self.cancellation.check().map_err(|_| RtpError::Cancelled)?;
        *prepared = Some(path);
        Ok(address)
    }

    /// Ownership of every valid nonnegative fd is transferred, including on failure or repeat.
    /// A cancelled preparation can never be used again. The returned worker awaits ACTIVATED.
    pub fn start(&self, tun_fd: i32) -> Result<Arc<RtpEngine>, RtpError> {
        // Reject invalid descriptors before constructing OwnedFd (whose contract requires validity).
        if tun_fd < 0 || unsafe { libc::fcntl(tun_fd, libc::F_GETFD) } < 0 {
            return Err(RtpError::Configuration);
        }
        let tun = unsafe { OwnedFd::from_raw_fd(tun_fd) };
        let mut ready = self.prepared.lock().unwrap();
        self.cancellation.check().map_err(|_| RtpError::Cancelled)?;
        let path = ready.take().ok_or(RtpError::Consumed)?;
        let engine = path.start(tun).map_err(|_| RtpError::Configuration)?;
        // Holding `ready` serializes handoff with cancel's cleanup. If cancel raced with start,
        // stop this worker before returning it to a caller.
        if self.cancellation.check().is_err() {
            engine.stop();
            return Err(RtpError::Cancelled);
        }
        self.transferred.store(true, Ordering::Release);
        Ok(Arc::new(RtpEngine { engine }))
    }

    /// Signals blocking HTTP and candidate attempts immediately. Cleanup is a worker operation.
    pub fn cancel(&self) {
        if !self.transferred.load(Ordering::Acquire) {
            self.cancellation.cancel();
        }
    }

    /// Call off the UI thread after preparation returns, including when TUN establishment fails.
    pub fn discard(&self) {
        self.cancel();
        self.inputs.lock().unwrap().take();
        self.prepared.lock().unwrap().take();
    }
}

// A foreign-language finalizer can run on any thread. Never perform release HTTP there.
impl Drop for RtpPreparation {
    fn drop(&mut self) {
        self.cancel();
        if let Some(path) = self.prepared.get_mut().unwrap().take() {
            let _ = std::thread::Builder::new()
                .name("rtp-discard".into())
                .spawn(move || drop(path));
        }
    }
}

fn preparation_error(error: anyhow::Error) -> RtpError {
    // Only match static protocol errors; never return arbitrary OS messages or profile content.
    if error.chain().any(|e| {
        e.to_string() == "control request rejected"
            || e.to_string()
                .contains("invalid authenticated control response")
    }) {
        RtpError::Rejected
    } else if error.chain().any(|e| {
        e.downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
    }) {
        RtpError::Configuration
    } else {
        RtpError::Connectivity
    }
}

fn init_logging() {
    #[cfg(target_os = "android")]
    {
        use tracing_subscriber::layer::SubscriberExt;
        static LOGGING: std::sync::Once = std::sync::Once::new();
        LOGGING.call_once(|| {
            let subscriber = tracing_subscriber::registry()
                .with(tracing_subscriber::filter::LevelFilter::INFO)
                .with(paranoid_android::layer("cpxy-rtp").with_ansi(false));
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    struct Never;
    impl RtpPlatform for Never {
        fn protect_socket(&self, _: i32) -> bool {
            false
        }
    }
    const PROFILE: &str = r#"{"endpoint":"127.0.0.1:30000","dns_upstream":["223.5.5.5"],"dns_alternative":["8.8.8.8"]}"#;
    fn operation() -> Arc<RtpPreparation> {
        create_rtp_preparation(
            PROFILE.into(),
            vec![0; 32],
            uuid::Uuid::new_v4().to_string(),
            Box::new(Never),
        )
        .unwrap()
    }
    #[test]
    fn rejects_bad_input_before_opening_sockets() {
        for (key, id, profile) in [
            (vec![0; 31], uuid::Uuid::new_v4().to_string(), PROFILE),
            (vec![0; 32], uuid::Uuid::nil().to_string(), PROFILE),
            (vec![0; 32], "invalid".into(), PROFILE),
            (vec![0; 32], uuid::Uuid::new_v4().to_string(), "{}"),
        ] {
            assert!(create_rtp_preparation(profile.into(), key, id, Box::new(Never)).is_err());
        }
        assert!(validate_profile(PROFILE.into()).is_none());
        assert!(validate_profile(PROFILE.replace("30000", "0")).is_some());
        assert!(derive_key("short".into()).is_err());
        assert_eq!(derive_key("long enough password".into()).unwrap().len(), 32);
    }
    #[test]
    fn cancel_is_idempotent_and_prepare_is_once_only() {
        let op = operation();
        op.cancel();
        op.cancel();
        assert!(matches!(op.prepare(), Err(RtpError::Cancelled)));
        assert!(matches!(op.prepare(), Err(RtpError::Consumed)));
        op.discard();
        op.discard();
    }
    #[test]
    fn failed_start_closes_transferred_fd_including_on_repeat() {
        use std::os::fd::{AsRawFd, IntoRawFd};
        let op = operation();
        for _ in 0..2 {
            let (fd, _other) = std::os::unix::net::UnixDatagram::pair().unwrap();
            let raw = fd.as_raw_fd();
            assert!(op.start(fd.into_raw_fd()).is_err());
            assert_eq!(unsafe { libc::fcntl(raw, libc::F_GETFD) }, -1);
        }
        assert!(op.start(-1).is_err());
    }
    #[test]
    fn false_socket_protection_aborts_preparation() {
        assert!(matches!(
            operation().prepare(),
            Err(RtpError::Configuration)
        ));
    }
}
