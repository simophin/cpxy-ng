//! Generic mTLS listener leases. No application keys, payloads or protocol epochs.
use crate::{Config, Mapping, Relay};
use port_admission::{Admission, Lease, LeaseState};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
/// Absolute I/O deadline prevents a slow peer extending a bounded control call
/// indefinitely by sending one byte before each socket timeout.
struct DeadlineStream {
    stream: TcpStream,
    deadline: std::time::Instant,
}
impl DeadlineStream {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            deadline: std::time::Instant::now() + Duration::from_secs(5),
        }
    }
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "management I/O deadline"))
    }
}
impl Read for DeadlineStream {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(b)
    }
}
impl Write for DeadlineStream {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut p = path.as_os_str().to_os_string();
    p.push(suffix);
    p.into()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn error(s: &str) -> io::Error {
    io::Error::other(s)
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tls {
    pub ca: PathBuf,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
}
fn certificates(p: &Path) -> io::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    rustls_pemfile::certs(&mut BufReader::new(File::open(p)?)).collect()
}
impl Tls {
    fn roots(&self) -> io::Result<rustls::RootCertStore> {
        let mut r = rustls::RootCertStore::empty();
        for c in certificates(&self.ca)? {
            r.add(c).map_err(io::Error::other)?;
        }
        Ok(r)
    }
    fn key(&self) -> io::Result<rustls::pki_types::PrivateKeyDer<'static>> {
        rustls_pemfile::private_key(&mut BufReader::new(File::open(&self.private_key)?))?
            .ok_or_else(|| error("missing private key"))
    }
    fn server(&self) -> io::Result<Arc<rustls::ServerConfig>> {
        let v = rustls::server::WebPkiClientVerifier::builder(Arc::new(self.roots()?))
            .build()
            .map_err(io::Error::other)?;
        Ok(Arc::new(
            rustls::ServerConfig::builder()
                .with_client_cert_verifier(v)
                .with_single_cert(certificates(&self.certificate)?, self.key()?)
                .map_err(io::Error::other)?,
        ))
    }
    fn client(&self) -> io::Result<Arc<rustls::ClientConfig>> {
        Ok(Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(self.roots()?)
                .with_client_auth_cert(certificates(&self.certificate)?, self.key()?)
                .map_err(io::Error::other)?,
        ))
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub address: SocketAddr,
    pub server_name: String,
    pub tls: Tls,
    pub owner: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Authority {
    pub owner: String,
    pub listen_ip: IpAddr,
    pub upstream_ip: IpAddr,
    pub ports: Vec<u16>,
    pub max_flows: usize,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedConfig {
    pub relay: Config,
    pub management: SocketAddr,
    pub tls: Tls,
    pub authority: Authority,
    pub journal: PathBuf,
    pub agent_socket: Option<PathBuf>,
    pub staged_seconds: u64,
    pub outage_seconds: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub owner: String,
    pub generation: String,
    pub listen: SocketAddr,
    pub upstream: SocketAddr,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    #[serde(default)]
    pub retiring: bool,
    pub spec: Spec,
    pub committed: bool,
    pub expires: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Inventory,
    Prepare { spec: Spec },
    Commit { spec: Spec, boot: String },
    Renew { spec: Spec, boot: String },
    Retire { spec: Spec },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Reply {
    pub boot: String,
    pub ready: bool,
    pub entries: Vec<Entry>,
    pub error: Option<String>,
}
/// A dedicated CA is the management authority for this one delegated owner.
/// No transport PSK is accepted by this API. One request per bounded connection.
pub fn call(config: &ClientConfig, request: &Request) -> io::Result<Reply> {
    let stream = TcpStream::connect_timeout(&config.address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let name = rustls::pki_types::ServerName::try_from(config.server_name.clone())
        .map_err(io::Error::other)?;
    let connection =
        rustls::ClientConnection::new(config.tls.client()?, name).map_err(io::Error::other)?;
    let mut tls = rustls::StreamOwned::new(connection, DeadlineStream::new(stream));
    serde_json::to_writer(&mut tls, request)?;
    tls.write_all(b"\n")?;
    tls.flush()?;
    let mut line = String::new();
    BufReader::new(tls).take(65536).read_line(&mut line)?;
    let reply: Reply = serde_json::from_str(&line)?;
    if let Some(e) = &reply.error {
        return Err(error(e));
    }
    Ok(reply)
}
enum Command {
    Disable(String, String, mpsc::SyncSender<io::Result<()>>),
    Install(Mapping, UdpSocket, mpsc::SyncSender<io::Result<()>>),
    Remove(String, String, mpsc::SyncSender<io::Result<()>>),
}
struct Manager {
    config: ManagedConfig,
    boot: String,
    entries: Vec<Entry>,
    admission: Box<dyn Admission>,
    tx: mpsc::SyncSender<Command>,
    _lock: File,
    healthy: bool,
    installed: HashSet<String>,
}
impl Manager {
    fn lease(&self, s: &Spec) -> Lease {
        Lease {
            profile: s.owner.clone(),
            generation: s.generation.clone(),
            address: s.listen,
            state: LeaseState::Prepared,
        }
    }
    fn save(&self) -> io::Result<()> {
        let p = sibling(&self.config.journal, ".new");
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&p)?;
        f.write_all(&serde_json::to_vec(&self.entries)?)?;
        f.sync_all()?;
        fs::rename(p, &self.config.journal)?;
        File::open(
            self.config
                .journal
                .parent()
                .ok_or_else(|| error("journal parent"))?,
        )?
        .sync_all()
    }
    fn validate(&self, s: &Spec) -> io::Result<()> {
        let a = &self.config.authority;
        if s.owner != a.owner
            || s.generation.is_empty()
            || s.generation.len() > 128
            || s.listen.ip() != a.listen_ip
            || s.upstream.ip() != a.upstream_ip
            || !a.ports.contains(&s.listen.port())
            || !a.ports.contains(&s.upstream.port())
        {
            return Err(error("outside delegated authority"));
        }
        Ok(())
    }
    fn install(&mut self, s: &Spec) -> io::Result<()> {
        let socket = crate::socket(s.listen, self.config.relay.socket_buffer)?;
        self.admission.prepare_port(&self.lease(s))?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .send(Command::Install(
                Mapping {
                    id: s.owner.clone(),
                    generation: s.generation.clone(),
                    listen: s.listen,
                    upstream: s.upstream,
                    max_flows: self.config.authority.max_flows,
                },
                socket,
                tx,
            ))
            .map_err(io::Error::other)?;
        rx.recv().map_err(io::Error::other)??;
        self.installed.insert(s.generation.clone());
        Ok(())
    }
    fn retire(&mut self, s: &Spec) -> io::Result<()> {
        // Exact generation matching: a late retire never releases a replacement.
        let Some(i) = self.entries.iter().position(|e| e.spec == *s) else {
            return Ok(());
        };
        self.entries[i].retiring = true;
        self.save()?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .send(Command::Disable(s.owner.clone(), s.generation.clone(), tx))
            .map_err(io::Error::other)?;
        rx.recv().map_err(io::Error::other)??;
        self.admission.retire_port(&self.lease(s))?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .send(Command::Remove(s.owner.clone(), s.generation.clone(), tx))
            .map_err(io::Error::other)?;
        rx.recv().map_err(io::Error::other)??;
        self.entries.remove(i);
        self.installed.remove(&s.generation);
        self.save()
    }
    fn reconcile(&mut self) -> io::Result<()> {
        for s in self
            .entries
            .iter()
            .filter(|e| e.expires <= now() || e.retiring)
            .map(|e| e.spec.clone())
            .collect::<Vec<_>>()
        {
            self.retire(&s)?;
        }
        for e in self.entries.clone() {
            if !self.installed.contains(&e.spec.generation) {
                self.install(&e.spec)?;
            }
        }
        self.admission.reconcile_ports(
            &self
                .entries
                .iter()
                .map(|e| self.lease(&e.spec))
                .collect::<Vec<_>>(),
        )
    }
    fn request(&mut self, r: Request) -> io::Result<()> {
        match r {
            Request::Inventory => {}
            Request::Prepare { spec } => {
                self.validate(&spec)?;
                if self.entries.iter().any(|e| e.spec == spec) {
                    if !self
                        .entries
                        .iter()
                        .any(|e| e.spec == spec && e.expires > now() && !e.retiring)
                        || !self.installed.contains(&spec.generation)
                        || !self.healthy
                    {
                        return Err(error("admission unhealthy"));
                    }
                    return Ok(());
                }
                if !self.healthy {
                    return Err(error("admission unhealthy"));
                }
                if self
                    .entries
                    .iter()
                    .any(|e| e.spec.listen == spec.listen || e.spec.generation == spec.generation)
                {
                    return Err(error("generation conflict"));
                }
                self.entries.push(Entry {
                    retiring: false,
                    spec: spec.clone(),
                    committed: false,
                    expires: now() + self.config.staged_seconds,
                });
                self.save()?;
                if let Err(e) = self.install(&spec) {
                    let _ = self.retire(&spec);
                    self.healthy = false;
                    return Err(e);
                }
                eprintln!(
                    "{}",
                    serde_json::json!({"event":"lease_prepared","port":spec.listen.port(),"generation":spec.generation,"boot":self.boot})
                );
            }
            Request::Renew { spec, boot } => {
                if !self.entries.iter().any(|e| e.spec == spec && e.committed) {
                    return Err(error("renew requires committed generation"));
                }
                self.request(Request::Commit { spec, boot })?;
            }
            Request::Commit { spec, boot } => {
                self.validate(&spec)?;
                if boot != self.boot || !self.healthy {
                    return Err(error("stale boot or admission unhealthy"));
                }
                if !self
                    .entries
                    .iter()
                    .any(|e| e.spec == spec && e.expires > now() && !e.retiring)
                {
                    return Err(error("unknown or expired generation"));
                }
                let lease = self.lease(&spec);
                self.admission.commit_port(&lease)?;
                let e = self.entries.iter_mut().find(|e| e.spec == spec).unwrap();
                e.committed = true;
                e.expires = now() + self.config.outage_seconds;
                self.save()?;
            }
            Request::Retire { spec } => {
                self.validate(&spec)?;
                self.retire(&spec)?;
            }
        };
        Ok(())
    }
}
/// Management and firewall work executes on this thread; only descriptor
/// install/remove commands run in the bounded packet poll.
pub fn run(config: ManagedConfig, stop: &'static AtomicBool) -> io::Result<()> {
    config.relay.validate()?;
    if !config.relay.mappings.is_empty()
        || !config.journal.is_absolute()
        || config.authority.owner.is_empty()
    {
        return Err(error(
            "managed mode requires empty static mappings, absolute journal and owner",
        ));
    }
    if config.authority.ports.is_empty()
        || config.authority.ports.len() > 64
        || config.authority.ports.contains(&0)
        || config.authority.max_flows == 0
        || config.authority.max_flows > config.relay.max_flows
        || !(2..=120).contains(&config.staged_seconds)
        || !(10..=86400).contains(&config.outage_seconds)
    {
        return Err(error("invalid managed limits"));
    }
    let mut relay = Relay::bind(config.relay.clone())?;
    let tls = config.tls.server()?;
    let listener = TcpListener::bind(config.management)?;
    listener.set_nonblocking(true)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(sibling(&config.journal, ".lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(error("journal locked"));
    }
    let entries: Vec<Entry> = match fs::read(&config.journal) {
        Ok(b) => serde_json::from_slice(&b)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => vec![],
        Err(e) => return Err(e),
    };
    let (tx, rx) = mpsc::sync_channel(4);
    let admission: Box<dyn Admission> = match &config.agent_socket {
        Some(s) => Box::new(port_admission::agent::Client { socket: s.clone() }),
        None => Box::new(port_admission::AdministratorManaged),
    };
    let mut manager = Manager {
        config,
        boot: format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ),
        entries,
        admission,
        tx,
        _lock: lock,
        healthy: false,
        installed: HashSet::new(),
    };
    // Restore live generations before serving RPC. Expired journals are retired.
    let thread = std::thread::spawn(move || -> io::Result<()> {
        for e in manager.entries.clone() {
            manager.validate(&e.spec)?;
            if e.expires > now() && !e.retiring {
                manager.install(&e.spec)?;
            } else {
                manager.retire(&e.spec)?;
            }
        }
        manager.reconcile()?;
        manager.healthy = true;
        eprintln!(
            "{}",
            serde_json::json!({"event":"management_ready","boot":manager.boot})
        );
        let mut checked = std::time::Instant::now();
        while !stop.load(Ordering::Relaxed) {
            if checked.elapsed() >= Duration::from_secs(1) {
                manager.healthy = manager.reconcile().is_ok();
                checked = std::time::Instant::now();
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                    let conn =
                        rustls::ServerConnection::new(tls.clone()).map_err(io::Error::other)?;
                    let mut stream = rustls::StreamOwned::new(conn, DeadlineStream::new(stream));
                    let result = (|| -> io::Result<()> {
                        let mut line = String::new();
                        BufReader::new((&mut stream).take(65536)).read_line(&mut line)?;
                        let request: Request = serde_json::from_str(&line)?;
                        let result = manager.request(request);
                        let reply = Reply {
                            boot: manager.boot.clone(),
                            ready: manager.healthy,
                            entries: manager.entries.clone(),
                            error: result.err().map(|e| e.to_string()),
                        };
                        serde_json::to_writer(&mut stream, &reply)?;
                        stream.write_all(b"\n")?;
                        stream.flush()
                    })();
                    if let Err(e) = result {
                        eprintln!(
                            "{}",
                            serde_json::json!({"event":"management_connection_rejected","error":e.to_string()})
                        );
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => return Err(e),
            }
        }
        for e in manager.entries.clone() {
            manager.retire(&e.spec)?;
        }
        Ok(())
    });
    let mut reported = std::time::Instant::now();
    loop {
        while let Ok(c) = rx.try_recv() {
            match c {
                Command::Disable(id, g, reply) => {
                    relay.disable(&id, &g);
                    let _ = reply.send(Ok(()));
                }
                Command::Install(m, s, reply) => {
                    let _ = reply.send(relay.install(m, s));
                }
                Command::Remove(id, g, reply) => {
                    relay.remove(&id, &g);
                    let _ = reply.send(Ok(()));
                }
            }
        }
        relay.tick();
        if reported.elapsed() >= Duration::from_secs(10) {
            eprintln!(
                "{}",
                serde_json::json!({"event":"inventory","inventory":relay.inventory()})
            );
            reported = std::time::Instant::now();
        }
        if thread.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    eprintln!(
        "{}",
        serde_json::json!({"event":"stopped","inventory":relay.inventory()})
    );
    thread
        .join()
        .map_err(|_| error("management thread panicked"))?
}

/// Independent post-crash cleanup. The exclusive journal lock rejects a live
/// relay; only exact persisted generations are submitted to the local agent.
/// A failed release stays journaled for a later retry. No TLS listener is opened.
pub fn cleanup(config: ManagedConfig) -> io::Result<()> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(sibling(&config.journal, ".lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(error("journal locked"));
    }
    let entries: Vec<Entry> = match fs::read(&config.journal) {
        Ok(b) => serde_json::from_slice(&b)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => vec![],
        Err(e) => return Err(e),
    };
    let admission: Box<dyn Admission> = match &config.agent_socket {
        Some(s) => Box::new(port_admission::agent::Client { socket: s.clone() }),
        None => Box::new(port_admission::AdministratorManaged),
    };
    let (tx, _rx) = mpsc::sync_channel(1);
    let mut manager = Manager {
        config,
        boot: String::new(),
        entries,
        admission,
        tx,
        _lock: lock,
        healthy: false,
        installed: HashSet::new(),
    };
    for e in &manager.entries {
        manager.validate(&e.spec)?;
    }
    let mut result = Ok(());
    for e in manager.entries.clone() {
        match manager.admission.retire_port(&manager.lease(&e.spec)) {
            Ok(()) => {
                manager.entries.retain(|l| l.spec != e.spec);
                manager.save()?;
            }
            Err(error) => result = Err(error),
        }
    }
    manager.save()?;
    result
}
