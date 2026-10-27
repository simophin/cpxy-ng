//! Managed Linux RTP service. The controller owns host setup and admission;
//! the packet worker receives descriptors and a restricted socket-provider API.
//! Workers use threads, not an OS privilege sandbox. Endpoints manage firewalld
//! in-process when it is present; otherwise port policy is the administrator's.
pub mod addresses;
pub mod broker;
pub mod cli;
pub mod config;
pub mod forwarding;
pub mod http_control;
pub mod network;
pub mod ports;

use config::{FirewallMode, Role, Settings};
use port_admission::firewalld::{DbusFirewall, Firewall, ManagedFirewall, Policy};
use ports::{AdministratorManaged, Admission, LeaseManager};
/// Interface carrying `ip`, or the matching family's default-route interface.
fn listen_interface(ip: std::net::IpAddr) -> io::Result<String> {
    let output = if ip.is_unspecified() {
        Command::new("ip")
            .args([
                if ip.is_ipv4() { "-4" } else { "-6" },
                "route",
                "show",
                "default",
            ])
            .output()?
    } else {
        Command::new("ip")
            .args(["-o", if ip.is_ipv4() { "-4" } else { "-6" }, "addr", "show"])
            .output()?
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let found = if ip.is_unspecified() {
        text.lines()
            .find_map(|l| l.split_whitespace().skip_while(|w| *w != "dev").nth(1))
    } else {
        let needle = format!("{ip}/");
        text.lines()
            .find(|l| l.split_whitespace().any(|w| w.starts_with(&needle)))
            .and_then(|l| l.split_whitespace().nth(1))
    };
    found
        .map(|name| name.split('@').next().unwrap_or(name).to_owned())
        .ok_or_else(|| {
            io::Error::other(format!(
                "no interface carries {ip}; pass --firewalld-interface"
            ))
        })
}
/// Firewalld policy when it manages this endpoint; `None` leaves admission to
/// the administrator. An explicit `firewalld` request fails when it is absent.
fn firewalld_policy(settings: &Settings) -> io::Result<Option<Policy>> {
    let Role::Endpoint {
        listen,
        udp_low,
        udp_high,
        ..
    } = settings.transport
    else {
        return Ok(None);
    };
    match settings.firewall {
        FirewallMode::None => return Ok(None),
        FirewallMode::Auto if !DbusFirewall::available() => return Ok(None),
        FirewallMode::Firewalld if !DbusFirewall::available() => {
            return Err(io::Error::other(
                "firewalld requested but not answering on the system bus",
            ))
        }
        _ => {}
    }
    let options = &settings.firewalld;
    let interface = match &options.interface {
        Some(interface) => interface.clone(),
        None => listen_interface(listen.ip())?,
    };
    let zone = match &options.zone {
        Some(zone) => zone.clone(),
        None => DbusFirewall.zone(&interface)?,
    };
    // Automatic sockets are admitted only after bind(:0) reveals their actual
    // port. This is delegation of individual leases, never a range-wide rule.
    let (low, high) = if udp_low == 0 {
        (1, u16::MAX)
    } else {
        (udp_low, udp_high)
    };
    Ok(Some(Policy {
        profile: settings.profile.clone(),
        bind: listen.ip(),
        base: low,
        low,
        high,
        zone,
        interface,
        sources: options.sources.clone(),
    }))
}
fn firewalld_journal(settings: &Settings) -> std::path::PathBuf {
    let mut path = settings.journal.as_os_str().to_os_string();
    path.push(".firewalld");
    path.into()
}
fn admission(settings: &Settings, events: &Events) -> io::Result<(Box<dyn Admission>, bool)> {
    let policy = firewalld_policy(settings)?;
    let managed = policy.is_some();
    if matches!(settings.transport, Role::Endpoint { .. }) {
        events(match &policy {
            Some(p) => {
                serde_json::json!({"event":"admission", "mode":"firewalld", "zone":p.zone, "interface":p.interface})
            }
            None => {
                serde_json::json!({"event":"admission", "mode":"administrator", "detail":"firewalld not managed; admit the TCP control port and allocated UDP ports externally"})
            }
        });
    }
    let local: Box<dyn Admission> = match policy {
        Some(policy) => Box::new(ManagedFirewall::open(
            vec![policy],
            DbusFirewall,
            firewalld_journal(settings),
        )?),
        None => Box::new(AdministratorManaged),
    };
    Ok((local, managed))
}

use rtp_l3::linux::{self, Events, SocketProvider};
use std::{
    fs::{File, OpenOptions},
    io,
    net::{SocketAddr, UdpSocket},
    os::fd::AsRawFd,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
};

fn ip(args: &[&str]) -> io::Result<()> {
    let result = Command::new("ip").args(args).output()?;
    if result.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "ip failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )))
    }
}
fn tun(settings: &Settings) -> io::Result<File> {
    // Refuse an existing interface; cleanup must never delete administrator state.
    if Command::new("ip")
        .args(["link", "show", "dev", &settings.interface])
        .output()?
        .status
        .success()
    {
        return Err(io::Error::other("TUN name already exists"));
    }
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")?;
    let mut req = [0u8; 40];
    req[..settings.interface.len()].copy_from_slice(settings.interface.as_bytes());
    req[16..18].copy_from_slice(&(0x9001u16).to_ne_bytes());
    if unsafe { libc::ioctl(f.as_raw_fd(), 0x400454ca, req.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // Nonpersistent TUN lifetime is owned by f, even if an ip command fails.
    ip(&["addr", "add", &settings.address, "dev", &settings.interface])?;
    ip(&["link", "set", "dev", &settings.interface, "mtu", "1200"])?;
    Ok(f)
}
fn client_socket(settings: &Settings, peer: SocketAddr) -> io::Result<UdpSocket> {
    let Role::Client { entry, bind } = settings.transport else {
        return Err(io::Error::other("not a client profile"));
    };
    if peer.ip() != entry.ip() {
        return Err(io::Error::other("peer outside configured entry"));
    }
    let socket = UdpSocket::bind(bind)?;
    broker::bind_device(&socket, settings.outbound_interface.as_deref())?;
    socket.connect(peer)?;
    Ok(socket)
}

/// Wildcard IPv6 endpoints explicitly accept both outer address families.
/// The option must be set before bind, including for every rotation socket.
fn endpoint_socket(address: SocketAddr) -> io::Result<UdpSocket> {
    let SocketAddr::V6(address6) = address else {
        return UdpSocket::bind(address);
    };
    if !address6.ip().is_unspecified() {
        return UdpSocket::bind(address);
    }
    use std::os::fd::FromRawFd;
    let fd = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let socket = unsafe { UdpSocket::from_raw_fd(fd) };
    let only: libc::c_int = 0;
    if unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IPV6,
            libc::IPV6_V6ONLY,
            (&only as *const libc::c_int).cast(),
            std::mem::size_of_val(&only) as _,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let addr = libc::sockaddr_in6 {
        sin6_family: libc::AF_INET6 as _,
        sin6_port: address6.port().to_be(),
        sin6_flowinfo: address6.flowinfo().to_be(),
        sin6_addr: libc::in6_addr {
            s6_addr: address6.ip().octets(),
        },
        sin6_scope_id: address6.scope_id(),
    };
    if unsafe {
        libc::bind(
            fd,
            (&addr as *const libc::sockaddr_in6).cast(),
            std::mem::size_of_val(&addr) as _,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(socket)
}
struct Host {
    settings: Settings,
    leases: LeaseManager,
    broker: Option<Arc<Mutex<File>>>,
    /// Firewalld forwarding for managed endpoints with service CIDRs.
    forwarding: Option<forwarding::Forwarding>,
    quarantine: Mutex<Vec<UdpSocket>>,
}
impl Host {
    fn release_tunnel(&self) -> io::Result<()> {
        self.forwarding.as_ref().map_or(Ok(()), |f| f.release())
    }
}
impl Host {
    fn open_client(&self, peer: SocketAddr) -> io::Result<UdpSocket> {
        match &self.broker {
            Some(broker) => {
                let Role::Client { entry, .. } = self.settings.transport else {
                    return Err(io::Error::other("broker requires client profile"));
                };
                if peer.ip() != entry.ip() || peer.port() == 0 {
                    return Err(io::Error::other("peer outside configured entry"));
                }
                let socket = linux::broker_socket(&broker.lock().unwrap(), peer.port())?;
                if socket.peer_addr()? != peer {
                    return Err(io::Error::other("broker returned wrong UDP peer"));
                }
                Ok(socket)
            }
            None => client_socket(&self.settings, peer),
        }
    }
}
impl SocketProvider for Host {
    fn control_socket(&self, peer: SocketAddr) -> io::Result<std::net::TcpStream> {
        let Role::Client { entry, bind } = self.settings.transport else {
            return Err(io::Error::other("not a client"));
        };
        if peer != entry {
            return Err(io::Error::other("control peer outside configured endpoint"));
        }
        match &self.broker {
            Some(channel) => {
                let socket = broker::tcp_from_broker(&channel.lock().unwrap(), peer.port())?;
                if socket.peer_addr()? != peer {
                    return Err(io::Error::other("wrong control socket peer"));
                }
                Ok(socket)
            }
            None => broker::tcp_socket(peer, bind, self.settings.outbound_interface.as_deref()),
        }
    }

    fn prepare_port(&self, address: SocketAddr) -> io::Result<UdpSocket> {
        let Role::Endpoint {
            listen,
            udp_low,
            udp_high,
            ..
        } = self.settings.transport
        else {
            return Err(io::Error::other("client cannot lease listeners"));
        };
        if address.ip() != listen.ip() || !(udp_low..=udp_high).contains(&address.port()) {
            return Err(io::Error::other("port outside profile namespace"));
        }
        let socket = endpoint_socket(address)?;
        // Port zero is a bind request, never a firewall/journal port.
        let allocated_address = socket.local_addr()?;
        if let Err(e) = self.leases.prepare_port(allocated_address) {
            if self.leases.retire_port(allocated_address.port()).is_err() {
                self.quarantine.lock().unwrap().push(socket);
            }
            return Err(e);
        }
        Ok(socket)
    }
    fn commit_port(&self, port: u16) -> io::Result<()> {
        self.leases.commit_port(port)
    }
    fn retire_port(&self, port: u16) -> io::Result<()> {
        self.leases.retire_port(port)
    }
    fn reconcile_ports(&self) -> io::Result<()> {
        self.leases.reconcile_ports()?;
        self.forwarding.as_ref().map_or(Ok(()), |f| f.restore())
    }
    fn client_socket(&self, peer: SocketAddr) -> io::Result<UdpSocket> {
        let socket = self.open_client(peer)?;
        // Runtime uses send_to across ports after bootstrap; retain a local tuple
        // and remove the connected filter using the proven core helper.
        linux::unconnect(&socket)?;
        Ok(socket)
    }
    fn packet_ready(&self) -> io::Result<()> {
        ip(&["link", "set", "dev", &self.settings.interface, "up"])
    }
}
#[derive(Clone, Debug)]
pub struct Health {
    pub state: String,
    pub last_event: serde_json::Value,
}
struct Active {
    running: Arc<AtomicBool>,
    thread: JoinHandle<io::Result<()>>,
    host: Arc<Host>,
}
pub struct Service {
    settings: Settings,
    psk: [u8; 32],
    active: Option<Active>,
    health: Arc<Mutex<Health>>,
    events: Events,
    broker: Option<Arc<Mutex<File>>>,
}
impl Service {
    /// `psk` comes from [`rtp_l3::derive_psk`]; the service never stores passwords.
    pub fn new(settings: Settings, psk: [u8; 32], events: Events) -> io::Result<Self> {
        settings.validate()?;
        Ok(Self {
            settings,
            psk,
            active: None,
            health: Arc::new(Mutex::new(Health {
                state: "stopped".into(),
                last_event: serde_json::Value::Null,
            })),
            events,
            broker: None,
        })
    }
    /// Use a private inherited host socket broker for every client socket.
    pub fn new_with_socket_broker(
        settings: Settings,
        psk: [u8; 32],
        events: Events,
        broker: File,
    ) -> io::Result<Self> {
        if !matches!(settings.transport, Role::Client { .. }) {
            return Err(io::Error::other("socket broker requires client profile"));
        }
        let mut service = Self::new(settings, psk, events)?;
        service.broker = Some(Arc::new(Mutex::new(broker)));
        Ok(service)
    }
    pub fn health(&self) -> Health {
        self.health.lock().unwrap().clone()
    }
    pub fn is_running(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|a| !a.thread.is_finished())
    }
    pub fn start(&mut self) -> io::Result<()> {
        if self.active.is_some() {
            return Err(io::Error::other(
                "service already started; stop before restart",
            ));
        }
        self.settings.validate()?;
        let psk = self.psk;
        let (admission, firewalld) = admission(&self.settings, &self.events)?;
        let forwarding = if firewalld && self.settings.network.is_some() {
            forwarding::cleanup(&self.settings)?;
            Some(forwarding::Forwarding::plan(&self.settings)?)
        } else {
            None
        };
        let host = Arc::new(Host {
            settings: self.settings.clone(),
            broker: self.broker.clone(),
            leases: LeaseManager::open(
                self.settings.journal.clone(),
                self.settings.profile.clone(),
                admission,
            )?,
            forwarding,
            quarantine: Mutex::new(Vec::new()),
        });
        let mut control_client = if let Role::Client { entry, .. } = self.settings.transport {
            Some(http_control::client::Client::new(
                &psk,
                &self.settings.control.server_identity,
                entry,
                host.clone(),
                addresses::client_id(&self.settings.journal.with_extension("client-id"))?,
            )?)
        } else {
            None
        };
        if self.settings.control.dynamic_address {
            let client = control_client
                .as_mut()
                .ok_or_else(|| io::Error::other("dynamic address is client-only"))?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let value = loop {
                match client.call("allocate_ip", Default::default()) {
                    Ok(value) => break value,
                    Err(e)
                        if std::time::Instant::now() < deadline
                            && matches!(
                                e.kind(),
                                io::ErrorKind::ConnectionRefused
                                    | io::ErrorKind::TimedOut
                                    | io::ErrorKind::NotConnected
                                    | io::ErrorKind::BrokenPipe
                                    | io::ErrorKind::WouldBlock
                                    | io::ErrorKind::ConnectionReset
                                    | io::ErrorKind::ConnectionAborted
                                    | io::ErrorKind::UnexpectedEof
                            ) =>
                    {
                        thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Err(e) => return Err(e),
                }
            };
            let inner: std::net::Ipv4Addr = serde_json::from_value(value["inner"].clone())?;
            let prefix = value["prefix"]
                .as_u64()
                .filter(|p| *p <= 30)
                .ok_or_else(|| io::Error::other("invalid allocated subnet"))?
                as u8;
            self.settings.address = format!("{inner}/{prefix}");
            if let Some(network) = &mut self.settings.network {
                network.client_addresses = vec![inner];
                let mask = if prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - prefix)
                };
                network.tunnel_pool = format!(
                    "{}/{prefix}",
                    std::net::Ipv4Addr::from(u32::from(inner) & mask)
                );
            }
            self.settings.validate()?;
        }
        let setup = (|| {
            network::prepare(&self.settings)?;
            let packet = tun(&self.settings)?;
            if let Some(forwarding) = &host.forwarding {
                forwarding.apply()?;
            }
            if self.settings.network.is_some() {
                ip(&["link", "set", "dev", &self.settings.interface, "up"])?;
            }
            network::activate(&self.settings)?;
            Ok::<_, io::Error>(packet)
        })();
        let packet = match setup {
            Ok(v) => v,
            Err(e) => {
                host.release_tunnel()?;
                host.leases.cleanup()?;
                return Err(e);
            }
        };
        let client = matches!(self.settings.transport, Role::Client { .. });
        let settings = self.settings.clone();
        let running = Arc::new(AtomicBool::new(true));
        let run = running.clone();
        let worker_host = host.clone();
        let health = self.health.clone();
        let events = self.events.clone();
        health.lock().unwrap().state = "starting".into();
        let worker_events: Events = Arc::new(move |event| {
            {
                let mut h = health.lock().unwrap();
                match event["event"].as_str() {
                    Some("ready") if !client => h.state = "ready".into(),
                    Some("connected" | "repaired") => h.state = "connected".into(),
                    Some("degraded") => h.state = "degraded".into(),
                    Some("repairing") => h.state = "repairing".into(),
                    Some("admission_restored") => {
                        h.state = if client {
                            if event["connected"] == true {
                                "connected"
                            } else {
                                "starting"
                            }
                        } else {
                            "ready"
                        }
                        .into()
                    }
                    _ => {}
                }
                h.last_event = event.clone();
            }
            events(event);
        });
        let final_health = self.health.clone();
        let final_events = self.events.clone();
        let thread = thread::Builder::new().name(format!("rtp-{}", self.settings.profile)).spawn(move || {
            let result = network::write_policy(&settings).and_then(|accept| if client {
                http_control::transport::run(settings,control_client.unwrap(),packet.into(),accept,worker_host.clone(),worker_events,run)
            } else {
                let device: Box<dyn linux::PacketIo> = Box::new(network::PolicyPacket{file:packet,accept});
                http_control::server::run(settings,psk,device,worker_host.clone(),worker_events,run)
            });
            let cleanup = worker_host.leases.cleanup();
            let result = result.and(cleanup).and(worker_host.release_tunnel());
            final_health.lock().unwrap().state = if result.is_ok() { "stopped" } else { "failed" }.into();
            final_events(serde_json::json!({"event":"stopped", "error":result.as_ref().err().map(ToString::to_string)}));
            result
        });
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => {
                self.health.lock().unwrap().state = "failed".into();
                host.release_tunnel()?;
                host.leases.cleanup()?;
                return Err(error);
            }
        };
        self.active = Some(Active {
            running,
            thread,
            host,
        });
        Ok(())
    }
    pub fn stop(&mut self) -> io::Result<()> {
        self.stop_inner(true)
    }
    fn stop_inner(&mut self, release_routes: bool) -> io::Result<()> {
        let Some(active) = self.active.take() else {
            return Ok(());
        };
        active.running.store(false, Ordering::Relaxed);
        let result = active
            .thread
            .join()
            .map_err(|_| io::Error::other("packet worker panicked"))
            .and_then(|v| v);
        let cleanup = active
            .host
            .leases
            .cleanup()
            .and(active.host.release_tunnel());
        // Unexpected worker failure retains unreachable guards. Explicit stop
        // releases owned routing policy after the TUN descriptor is closed.
        let network_cleanup = if result.is_ok() && release_routes {
            network::cleanup(&self.settings)
        } else {
            Ok(())
        };
        result.and(cleanup).and(network_cleanup)
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub fn cleanup_ports(settings: &Settings, events: &Events) -> io::Result<()> {
    settings.validate()?;
    let (admission, _) = admission(settings, events)?;
    LeaseManager::open(
        settings.journal.clone(),
        settings.profile.clone(),
        admission,
    )?
    .cleanup()
}
pub fn cleanup(settings: &Settings, events: &Events) -> io::Result<()> {
    settings.validate()?;
    let (admission, firewalld) = admission(settings, events)?;
    LeaseManager::open(
        settings.journal.clone(),
        settings.profile.clone(),
        admission,
    )?
    .cleanup()?;
    if firewalld {
        forwarding::cleanup(settings)?;
    }
    network::cleanup(settings)
}
/// The only source of the shared password; never a flag, so it stays out of argv.
pub const PASSWORD_ENV: &str = "CPXY_RTP_PASSWORD";
/// Read-only diagnostics: no keys, routes or firewall mutations.
pub fn doctor(settings: &Settings) -> serde_json::Value {
    let route = match settings.transport {
        Role::Client { entry, .. } => Command::new("ip")
            .args([
                if entry.is_ipv4() { "-4" } else { "-6" },
                "route",
                "get",
                &entry.ip().to_string(),
            ])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()),
        _ => None,
    };
    let socket = match settings.transport {
        Role::Client { entry, .. } => client_socket(settings, entry.into()),
        Role::Endpoint {
            listen, udp_low, ..
        } => {
            let mut address = listen;
            address.set_port(udp_low);
            endpoint_socket(address)
        }
    };
    serde_json::json!({"configuration":settings.validate().err().map(|e|e.to_string()), "password_env":std::env::var_os(PASSWORD_ENV).is_some(), "tun_device":std::path::Path::new("/dev/net/tun").exists(), "routing":route, "socket_bind":socket.err().map(|e|e.to_string()), "firewall":{"mode":settings.firewall,"firewalld_available":DbusFirewall::available(),"policy":firewalld_policy(settings).map(|p| p.map(|p| serde_json::json!({"zone":p.zone,"interface":p.interface,"sources":p.sources}))).map_err(|e| e.to_string()),"tunnel_zone":settings.firewalld.tunnel_zone,"masquerade":settings.firewalld.masquerade,"verified":false,"detail":"read-only diagnostics do not assert admission; the running service verifies rules before readiness and periodically"}, "network":settings.network,"dns_policy":{"mode":"externally_managed_only","verified":false,"detail":"service never mutates DNS; administrator must provision persistent fail-closed private suffix routing"},"fail_closed":"configured CIDRs use reserved policy rules and unreachable guard; explicit stop/cleanup releases policy", "mtu":1200,"ipv4_only":true,"outer_transport":"ipv4_or_ipv6"})
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    #[test]
    fn automatic_port_is_bound_before_journaling_and_retired_by_actual_port() {
        use clap::Parser;
        let root =
            std::env::temp_dir().join(format!("rtp-automatic-admission-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let cli::Cli {
            role: cli::RoleCommand::Endpoint { args, .. },
        } = cli::Cli::parse_from([
            "rtp",
            "endpoint",
            "--listen",
            "127.0.0.1:30000",
            "--firewall",
            "none",
            "--state-dir",
            root.to_str().unwrap(),
        ])
        else {
            panic!()
        };
        let settings = args.settings().unwrap();
        let journal = settings.journal.clone();
        let host = Host {
            settings,
            leases: LeaseManager::open(
                journal.clone(),
                cli::NAME.into(),
                Box::new(AdministratorManaged),
            )
            .unwrap(),
            broker: None,
            forwarding: None,
            quarantine: Mutex::new(Vec::new()),
        };
        let socket = host.prepare_port("127.0.0.1:0".parse().unwrap()).unwrap();
        let actual = socket.local_addr().unwrap();
        assert_ne!(actual.port(), 0);
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
        assert_eq!(saved["leases"][0]["address"], actual.to_string());
        host.commit_port(actual.port()).unwrap();
        host.retire_port(actual.port()).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
        assert_eq!(saved["leases"], serde_json::json!([]));
        drop(socket);
        assert!(UdpSocket::bind(actual).is_ok());
        drop(host);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn wildcard_endpoint_accepts_both_families_on_one_port() {
        let endpoint = endpoint_socket("[::]:0".parse().unwrap()).unwrap();
        endpoint
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let port = endpoint.local_addr().unwrap().port();
        for (bind, destination) in [
            ("127.0.0.1:0", format!("127.0.0.1:{port}")),
            ("[::1]:0", format!("[::1]:{port}")),
        ] {
            let client = UdpSocket::bind(bind).unwrap();
            client
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            client.send_to(b"probe", &destination).unwrap();
            let mut buffer = [0; 16];
            let (n, peer) = endpoint.recv_from(&mut buffer).unwrap();
            assert_eq!(&buffer[..n], b"probe");
            endpoint.send_to(b"reply", peer).unwrap();
            assert_eq!(client.recv_from(&mut buffer).unwrap().0, 5);
        }
    }
}
