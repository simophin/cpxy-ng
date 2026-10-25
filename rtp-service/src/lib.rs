//! Managed Linux RTP service. The controller owns host setup and admission;
//! the packet worker receives descriptors and a restricted socket-provider API.
//! Workers use threads, not an OS privilege sandbox. Managed firewall admission
//! is delegated to the separate policy-restricted local agent.
pub mod config;
pub mod ports;

use config::{Role, Settings};
use ports::{AdministratorManaged, Admission, LeaseManager};
fn admission(settings: &Settings) -> Box<dyn Admission> {
    match &settings.admission {
        config::AdmissionMode::AdministratorManaged => Box::new(AdministratorManaged),
        config::AdmissionMode::Firewalld { agent_socket } => {
            Box::new(port_admission::agent::Client {
                socket: agent_socket.clone(),
            })
        }
    }
}
use rtp_l3::linux::{self, Events, Resources, SocketProvider, WorkerConfig};
use std::{
    fs::{File, OpenOptions},
    io,
    net::{SocketAddr, UdpSocket},
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
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
fn key(path: &std::path::Path) -> io::Result<[u8; 32]> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    use std::io::Read;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::other("key file must be private and regular"));
    }
    let mut bytes = Vec::new();
    file.take(33).read_to_end(&mut bytes)?;
    bytes
        .try_into()
        .map_err(|_| io::Error::other("key must be exactly 32 bytes"))
}
use std::os::unix::fs::OpenOptionsExt;
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
    if peer.ip() != *entry.ip() {
        return Err(io::Error::other("peer outside configured entry"));
    }
    let socket = UdpSocket::bind(bind)?;
    let device = std::ffi::CString::new(
        settings
            .outbound_interface
            .as_deref()
            .ok_or_else(|| io::Error::other("missing outbound_interface"))?,
    )
    .map_err(io::Error::other)?;
    if unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            device.as_ptr().cast(),
            device.as_bytes_with_nul().len() as _,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    socket.connect(peer)?;
    Ok(socket)
}
struct Host {
    settings: Settings,
    leases: LeaseManager,
}
impl SocketProvider for Host {
    fn prepare_port(&self, address: SocketAddr) -> io::Result<UdpSocket> {
        let Role::Endpoint {
            listen,
            rotation_low,
            rotation_high,
            ..
        } = self.settings.transport
        else {
            return Err(io::Error::other("client cannot lease listeners"));
        };
        if address.ip() != *listen.ip()
            || address.port() != listen.port()
                && !(rotation_low..=rotation_high).contains(&address.port())
        {
            return Err(io::Error::other("port outside profile namespace"));
        }
        let socket = UdpSocket::bind(address)?;
        if let Err(e) = self.leases.prepare_port(address) {
            let _ = self.leases.retire_port(address.port());
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
        self.leases.reconcile_ports()
    }
    fn client_socket(&self, peer: SocketAddr) -> io::Result<UdpSocket> {
        let socket = client_socket(&self.settings, peer)?;
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
    active: Option<Active>,
    health: Arc<Mutex<Health>>,
    events: Events,
}
impl Service {
    pub fn new(settings: Settings, events: Events) -> io::Result<Self> {
        settings.validate()?;
        Ok(Self {
            settings,
            active: None,
            health: Arc::new(Mutex::new(Health {
                state: "stopped".into(),
                last_event: serde_json::Value::Null,
            })),
            events,
        })
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
        let psk = key(&self.settings.key_file)?;
        let host = Arc::new(Host {
            settings: self.settings.clone(),
            leases: LeaseManager::open(
                self.settings.journal.clone(),
                self.settings.profile.clone(),
                admission(&self.settings),
            )?,
        });
        let setup = (|| {
            let socket = match self.settings.transport {
                Role::Client { entry, .. } => client_socket(&self.settings, entry.into())?,
                Role::Endpoint { listen, .. } => {
                    let s = host.prepare_port(listen.into())?;
                    host.commit_port(listen.port())?;
                    s
                }
            };
            let packet = tun(&self.settings)?;
            Ok::<_, io::Error>((socket, packet))
        })();
        let (socket, packet) = match setup {
            Ok(v) => v,
            Err(e) => {
                host.leases.cleanup()?;
                return Err(e);
            }
        };
        let (inner, prefix) = self.settings.inner()?;
        let (client, low, high, rotation) = match self.settings.transport {
            Role::Client { .. } => (true, 0, 0, 0),
            Role::Endpoint {
                rotation_low,
                rotation_high,
                rotation_seconds,
                ..
            } => (false, rotation_low, rotation_high, rotation_seconds),
        };
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
            let result = linux::run(WorkerConfig { client, psk, inner, prefix, rotation_low: low, rotation_high: high, rotation_seconds: rotation, keepalive_seconds: worker_host.settings.keepalive_seconds }, Resources { packet: Box::new(packet), clock: Arc::new(linux::MonotonicClock::default()), socket, provider: worker_host.clone(), events: worker_events }, run);
            let cleanup = worker_host.leases.cleanup();
            let result = result.and(cleanup);
            final_health.lock().unwrap().state = if result.is_ok() { "stopped" } else { "failed" }.into();
            final_events(serde_json::json!({"event":"stopped", "error":result.as_ref().err().map(ToString::to_string)}));
            result
        });
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => {
                self.health.lock().unwrap().state = "failed".into();
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
        let Some(active) = self.active.take() else {
            return Ok(());
        };
        active.running.store(false, Ordering::Relaxed);
        let result = active
            .thread
            .join()
            .map_err(|_| io::Error::other("packet worker panicked"))
            .and_then(|v| v);
        let cleanup = active.host.leases.cleanup();
        result.and(cleanup)
    }
    /// Reload validates configuration and secrets before stopping. Role/interface
    /// changes establish a new session; this is a controlled restart.
    pub fn reload(&mut self, settings: Settings) -> io::Result<()> {
        settings.validate()?;
        key(&settings.key_file)?;
        self.stop()?;
        self.settings = settings;
        self.start()
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub fn cleanup(settings: &Settings) -> io::Result<()> {
    settings.validate()?;
    LeaseManager::open(
        settings.journal.clone(),
        settings.profile.clone(),
        admission(settings),
    )?
    .cleanup()
}
/// Read-only diagnostics: no keys, routes or firewall mutations.
pub fn doctor(settings: &Settings) -> serde_json::Value {
    let route = match settings.transport {
        Role::Client { entry, .. } => Command::new("ip")
            .args(["route", "get", &entry.ip().to_string()])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()),
        _ => None,
    };
    let socket = match settings.transport {
        Role::Client { entry, .. } => client_socket(settings, entry.into()),
        Role::Endpoint { listen, .. } => UdpSocket::bind(listen),
    };
    serde_json::json!({"configuration":settings.validate().err().map(|e|e.to_string()), "key":key(&settings.key_file).err().map(|e|e.to_string()), "tun_device":std::path::Path::new("/dev/net/tun").exists(), "routing":route, "socket_bind":socket.err().map(|e|e.to_string()), "firewall":{"mode":settings.admission,"verified":false,"detail":"read-only diagnostics do not assert admission; managed service verifies via agent before readiness and periodically"}, "mtu":1200,"ipv4_only":true})
}
