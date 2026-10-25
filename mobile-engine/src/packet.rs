//! Raw IPv4 application profiles. The platform owns persistent fail-closed routes
//! and DNS policy; this module owns the packet descriptor and protected UDP sockets.
use rtp_l3::worker::{self, SocketProvider};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    os::fd::{AsRawFd, OwnedFd},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

/// Existing proxy configuration remains accepted unchanged by `crate::start`.
#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ApplicationProfile {
    Proxy(crate::Config),
    RtpL3(PacketProfile),
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PacketProfile {
    /// Public IPv4 endpoint; advertised ports share this IP.
    pub entry: SocketAddr,
    /// Logical endpoint label bound to the secure key reference by the platform.
    pub endpoint_identity: String,
    /// Platform secure-storage reference to the shared password.
    pub password_reference: String,
    pub address: Ipv4Addr,
    pub routes: Vec<ipnet::Ipv4Net>,
    pub dns: DnsPolicy,
    #[serde(default = "mtu")]
    pub mtu: u16,
}
fn mtu() -> u16 {
    1200
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "policy", rename_all = "snake_case", deny_unknown_fields)]
pub enum DnsPolicy {
    None,
    /// Platform must prevent private suffix fallback even on crash/disconnect.
    ExternallyManagedFailClosed {
        resolvers: Vec<Ipv4Addr>,
        suffixes: Vec<String>,
    },
}
impl PacketProfile {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.entry.is_ipv4() && self.entry.port() != 0,
            "RTP requires an IPv4 entry"
        );
        anyhow::ensure!(self.mtu == 1200, "RTP IPv4 MTU must be 1200");
        anyhow::ensure!(
            !self.endpoint_identity.is_empty() && !self.password_reference.is_empty(),
            "Endpoint identity and secure password reference required"
        );
        anyhow::ensure!(
            !self.address.is_unspecified() && !self.address.is_multicast(),
            "Invalid tunnel address"
        );
        anyhow::ensure!(!self.routes.is_empty(), "Selected IPv4 routes required");
        anyhow::ensure!(
            !self
                .routes
                .iter()
                .any(|r| r.contains(&match self.entry.ip() {
                    std::net::IpAddr::V4(a) => a,
                    _ => unreachable!(),
                })),
            "Outer entry must be excluded from tunnel routes"
        );
        if let DnsPolicy::ExternallyManagedFailClosed {
            resolvers,
            suffixes,
        } = &self.dns
        {
            anyhow::ensure!(
                !resolvers.is_empty() && !suffixes.is_empty(),
                "Private DNS resolvers and suffixes required"
            );
            anyhow::ensure!(
                resolvers
                    .iter()
                    .all(|a| self.routes.iter().any(|r| r.contains(a))),
                "DNS resolvers must use selected routes"
            );
            for suffix in suffixes {
                client::dns_split::search::parse_search_domain(suffix)?;
            }
        }
        Ok(())
    }
}
/// Callbacks run on worker threads. Never retain the borrowed socket descriptor.
/// Network preparation must install persistent unreachable guards, selected routes,
/// and private DNS no-fallback policy before returning success. Errors are fatal.
pub trait Platform: Send + Sync + 'static {
    /// The shared password; the engine derives the key exactly as the endpoint does.
    fn resolve_password(&self, endpoint_identity: &str, reference: &str) -> io::Result<String>;
    fn prepare_network(&self, profile: &PacketProfile) -> io::Result<()>;
    /// Must protect/bind every new unconnected socket before any datagram is sent.
    fn protect_socket(&self, socket: &UdpSocket) -> io::Result<()>;
}
struct Provider(Arc<dyn Platform>);
impl SocketProvider for Provider {
    fn client_socket(&self, _peer: SocketAddr) -> io::Result<UdpSocket> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        self.0.protect_socket(&socket)?;
        Ok(socket)
    }
    fn prepare_port(&self, _: SocketAddr) -> io::Result<UdpSocket> {
        Err(io::Error::other("client only"))
    }
    fn commit_port(&self, _: u16) -> io::Result<()> {
        Err(io::Error::other("client only"))
    }
    fn retire_port(&self, _: u16) -> io::Result<()> {
        Err(io::Error::other("client only"))
    }
    fn reconcile_ports(&self) -> io::Result<()> {
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Connecting,
    Connected,
    Repairing,
    Degraded,
    Stopped,
}
#[derive(Clone, Serialize)]
pub struct Diagnostics {
    pub status: Status,
    pub endpoint_identity: String,
    pub entry: SocketAddr,
    pub ipv4_only: bool,
    pub mtu: u16,
    pub effective_routes: Vec<ipnet::Ipv4Net>,
    pub dns: DnsPolicy,
    pub network_owner: &'static str,
    pub counters: serde_json::Value,
    /// Bounded recent status changes; repairs can finish between status polls.
    pub transitions: std::collections::VecDeque<Status>,
    /// Worker failure as an `io::ErrorKind` only; messages may carry OS detail.
    pub error: Option<String>,
}
const TRANSITIONS: usize = 16;
impl Diagnostics {
    fn set_status(&mut self, status: Status) {
        if self.status != status {
            self.status = status;
            if self.transitions.len() == TRANSITIONS {
                self.transitions.pop_front();
            }
            self.transitions.push_back(status);
        }
    }
    fn observe(&mut self, event: serde_json::Value) {
        match event["event"].as_str() {
            Some("connected" | "repaired" | "path_alive") => self.set_status(Status::Connected),
            Some("repairing") => self.set_status(Status::Repairing),
            Some("degraded") => self.set_status(Status::Degraded),
            Some("stats" | "final") => {
                // Only numeric counters; never copy arbitrary worker events.
                self.counters = serde_json::Value::Object(
                    event
                        .as_object()
                        .unwrap()
                        .iter()
                        .filter(|(_, v)| v.is_number())
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                );
            }
            _ => {}
        }
    }
}
pub struct PacketEngine {
    running: Arc<AtomicBool>,
    task: Mutex<Option<JoinHandle<()>>>,
    diagnostics: Arc<Mutex<Diagnostics>>,
}
impl PacketEngine {
    /// A successful start means policy/socket setup succeeded; authenticated
    /// registration is asynchronous and reported through diagnostics.
    pub fn start(
        tun: OwnedFd,
        profile: PacketProfile,
        platform: Arc<dyn Platform>,
    ) -> anyhow::Result<Self> {
        profile.validate()?;
        // Never propagate platform errors: a keystore error may contain secrets.
        let psk = platform
            .resolve_password(&profile.endpoint_identity, &profile.password_reference)
            .ok()
            .and_then(|password| rtp_l3::derive_psk(&password).ok())
            .ok_or_else(|| anyhow::anyhow!("Secure key resolution failed"))?;
        platform
            .prepare_network(&profile)
            .map_err(|_| anyhow::anyhow!("Platform network policy failed"))?;
        let provider = Arc::new(Provider(platform));
        let socket = provider
            .client_socket(profile.entry)
            .map_err(|_| anyhow::anyhow!("Protected socket creation failed"))?;
        let running = Arc::new(AtomicBool::new(true));
        let diagnostics = Arc::new(Mutex::new(Diagnostics {
            status: Status::Connecting,
            endpoint_identity: profile.endpoint_identity.clone(),
            entry: profile.entry,
            ipv4_only: true,
            mtu: profile.mtu,
            effective_routes: profile.routes.clone(),
            dns: profile.dns.clone(),
            network_owner: "platform_persistent_fail_closed",
            counters: serde_json::json!({}),
            transitions: [Status::Connecting].into(),
            error: None,
        }));
        let events_diag = diagnostics.clone();
        let events = Arc::new(move |event: serde_json::Value| {
            events_diag.lock().unwrap().observe(event);
        });
        let worker_running = running.clone();
        let finished_diag = diagnostics.clone();
        let config = worker::WorkerConfig {
            client: true,
            psk,
            inner: profile.address,
            prefix: 32,
            rotation_low: 0,
            rotation_high: 0,
            initial_rotation_port: None,
            rotation_seconds: 0,
            keepalive_seconds: 5,
        };
        let packet = Box::new(PacketDevice {
            file: File::from(tun),
            profile: profile.clone(),
        });
        let task = std::thread::Builder::new()
            .name("rtp-packet".into())
            .spawn(move || {
                let result = worker::run_client(
                    config,
                    worker::Resources {
                        packet,
                        socket,
                        provider,
                        events,
                        clock: Arc::new(worker::MonotonicClock::default()),
                    },
                    worker_running.clone(),
                    profile.entry,
                );
                let mut diagnostics = finished_diag.lock().unwrap();
                match result {
                    Ok(()) => diagnostics.set_status(Status::Stopped),
                    Err(error) => {
                        diagnostics.error = Some(format!("{:?}", error.kind()));
                        diagnostics.set_status(Status::Degraded);
                    }
                }
                drop(diagnostics);
                worker_running.store(false, Ordering::Release);
            })?;
        Ok(Self {
            running,
            task: Mutex::new(Some(task)),
            diagnostics,
        })
    }
    pub fn diagnostics(&self) -> Diagnostics {
        self.diagnostics.lock().unwrap().clone()
    }
    /// Closes all transport/TUN descriptors. Platform guards and private DNS
    /// remain fail closed until explicit platform disconnect policy removes them.
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
        if let Some(task) = self.task.lock().unwrap().take() {
            let _ = task.join();
        }
        self.diagnostics.lock().unwrap().set_status(Status::Stopped);
    }
}
impl Drop for PacketEngine {
    fn drop(&mut self) {
        self.stop();
    }
}
struct PacketDevice {
    file: File,
    profile: PacketProfile,
}
impl AsRawFd for PacketDevice {
    fn as_raw_fd(&self) -> i32 {
        self.file.as_raw_fd()
    }
}
impl PacketDevice {
    fn allowed(&self, p: &[u8], outgoing: bool) -> bool {
        if p.len() < 20 || p[0] >> 4 != 4 {
            return false;
        }
        let src = Ipv4Addr::new(p[12], p[13], p[14], p[15]);
        let dst = Ipv4Addr::new(p[16], p[17], p[18], p[19]);
        let (own, remote) = if outgoing { (src, dst) } else { (dst, src) };
        own == self.profile.address && self.profile.routes.iter().any(|r| r.contains(&remote))
    }
}
impl Read for PacketDevice {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        for _ in 0..32 {
            let mut packet = [0u8; 65540];
            let n = self.file.read(&mut packet)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Packet source closed",
                ));
            }
            let prefix = if cfg!(any(target_os = "ios", target_os = "macos")) {
                4
            } else {
                0
            };
            if n < prefix {
                continue;
            }
            let p = &packet[prefix..n];
            if !self.allowed(p, true) || p.len() > 1200 || p.len() > b.len() {
                continue;
            }
            b[..p.len()].copy_from_slice(p);
            return Ok(p.len());
        }
        Err(io::ErrorKind::WouldBlock.into())
    }
}
impl Write for PacketDevice {
    fn write(&mut self, p: &[u8]) -> io::Result<usize> {
        if !self.allowed(p, false) {
            return Err(io::Error::other("Packet policy rejected"));
        }
        let mut packet = Vec::with_capacity(p.len() + 4);
        if cfg!(any(target_os = "ios", target_os = "macos")) {
            packet.extend((libc::AF_INET as u32).to_be_bytes());
        }
        packet.extend(p);
        let n = self.file.write(&packet)?;
        if n != packet.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "Partial packet write",
            ));
        }
        Ok(p.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Explicit profile dispatch; the legacy `start` and its JSON remain unchanged.
pub enum ApplicationEngine {
    Proxy(crate::EngineHandle),
    RtpL3(PacketEngine),
}
impl ApplicationEngine {
    pub fn stop(&self) {
        match self {
            Self::Proxy(h) => h.stop(),
            Self::RtpL3(h) => h.stop(),
        }
    }
}
pub fn start_profile(
    tun: OwnedFd,
    json: &str,
    listener: Arc<dyn crate::EventListener>,
    platform: Arc<dyn Platform>,
) -> anyhow::Result<ApplicationEngine> {
    match serde_json::from_str::<ApplicationProfile>(json)? {
        ApplicationProfile::Proxy(_) => {
            let mut value: serde_json::Value = serde_json::from_str(json)?;
            value.as_object_mut().unwrap().remove("mode");
            crate::start(tun, &value.to_string(), listener).map(ApplicationEngine::Proxy)
        }
        ApplicationProfile::RtpL3(profile) => {
            PacketEngine::start(tun, profile, platform).map(ApplicationEngine::RtpL3)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> PacketProfile {
        serde_json::from_str(r#"{"entry":"192.0.2.1:30000","endpoint_identity":"test","password_reference":"secure:reference","address":"10.88.0.2","routes":["10.90.0.0/24"],"dns":{"policy":"none"}}"#).unwrap()
    }
    #[test]
    fn modes_mtu_ipv4_and_private_dns() {
        let p = profile();
        p.validate().unwrap();
        let mut bad = p.clone();
        bad.mtu = 1280;
        assert!(bad.validate().is_err());
        bad = p.clone();
        bad.entry = "[::1]:30000".parse().unwrap();
        assert!(bad.validate().is_err());
        bad = p.clone();
        bad.routes = vec!["0.0.0.0/0".parse().unwrap()];
        assert!(bad.validate().is_err());
        bad = p.clone();
        bad.dns = DnsPolicy::ExternallyManagedFailClosed {
            resolvers: vec!["8.8.8.8".parse().unwrap()],
            suffixes: vec!["corp.example".into()],
        };
        assert!(bad.validate().is_err());
        let mut tagged = serde_json::to_value(&p).unwrap();
        tagged["mode"] = "rtp_l3".into();
        assert!(matches!(
            serde_json::from_value::<ApplicationProfile>(tagged).unwrap(),
            ApplicationProfile::RtpL3(_)
        ));
        let proxy = r#"{"mode":"proxy","server":"http://:key@1.2.3.4","dns_upstream":["1.1.1.1"],"dns_alternative":["8.8.8.8"],"mtu":1200}"#;
        let ApplicationProfile::Proxy(config) = serde_json::from_str(proxy).unwrap() else {
            panic!()
        };
        assert!(config.settings().is_err());
    }
    #[test]
    fn health_recovers_and_diagnostics_exclude_key_material() {
        let p = profile();
        let mut d = Diagnostics {
            status: Status::Connecting,
            endpoint_identity: p.endpoint_identity,
            entry: p.entry,
            ipv4_only: true,
            mtu: p.mtu,
            effective_routes: p.routes,
            dns: p.dns,
            network_owner: "platform",
            counters: serde_json::json!({}),
            transitions: [Status::Connecting].into(),
            error: None,
        };
        for (event, expected) in [
            ("degraded", Status::Degraded),
            ("path_alive", Status::Connected),
            ("repairing", Status::Repairing),
            ("degraded", Status::Degraded),
            ("repaired", Status::Connected),
        ] {
            d.observe(serde_json::json!({"event":event}));
            assert_eq!(d.status, expected);
        }
        d.observe(serde_json::json!({"event":"path_alive"}));
        assert_eq!(
            Vec::from(d.transitions.clone()),
            [
                Status::Connecting,
                Status::Degraded,
                Status::Connected,
                Status::Repairing,
                Status::Degraded,
                Status::Connected
            ]
        );
        d.observe(serde_json::json!({"event":"stats","fallbacks":7,"secret":"do-not-copy","password_reference":"secure:reference"}));
        assert!(matches!(d.status, Status::Connected));
        let output = serde_json::to_string(&d).unwrap();
        assert!(!output.contains("do-not-copy"));
        assert!(!output.contains("secure:reference"));
    }
}
