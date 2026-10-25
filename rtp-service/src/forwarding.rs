//! Runtime firewalld forwarding for endpoint service CIDRs: the service's own
//! TUN joins the tunnel zone, and optional masquerade rules are scoped to the
//! tunnel pool and each service CIDR in the zone of its egress interface.
//! Only state this module added is journaled, restored after a firewalld
//! reload, and removed on release; permanent configuration is never touched.
use crate::config::Settings;
use port_admission::firewalld::{DbusFirewall, Firewall};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
struct Owned {
    /// (zone, interface) bound by this service.
    tunnel: Option<(String, String)>,
    /// (zone, rich rule) added by this service.
    rules: Vec<(String, String)>,
}
pub struct Forwarding {
    path: PathBuf,
    tunnel: (String, String),
    rules: Vec<(String, String)>,
    owned: Mutex<Owned>,
}
fn journal(settings: &Settings) -> PathBuf {
    let mut path = settings.journal.as_os_str().to_os_string();
    path.push(".forwarding");
    path.into()
}
/// Interface the kernel uses to reach the first host of `cidr`.
fn egress_interface(cidr: &str) -> io::Result<String> {
    let (network, prefix) = cidr
        .split_once('/')
        .ok_or_else(|| io::Error::other("CIDR needs prefix"))?;
    let network: Ipv4Addr = network.parse().map_err(io::Error::other)?;
    let host = if prefix == "32" {
        network
    } else {
        Ipv4Addr::from(u32::from(network) + 1)
    };
    let output = Command::new("ip")
        .args(["-4", "route", "get", &host.to_string()])
        .output()?;
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .skip_while(|w| *w != "dev")
        .nth(1)
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other(format!("no route to service CIDR {cidr}")))
}
fn save(path: &Path, owned: &Owned) -> io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut temp = path.as_os_str().to_os_string();
    temp.push(".new");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)?;
    file.write_all(&serde_json::to_vec(owned)?)?;
    file.sync_all()?;
    fs::rename(temp, path)
}
fn load(path: &Path) -> io::Result<Owned> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Owned::default()),
        Err(e) => Err(e),
    }
}
fn remove(path: &Path, owned: &Owned) -> io::Result<()> {
    for (zone, rule) in &owned.rules {
        if DbusFirewall.query(zone, rule)? {
            DbusFirewall.remove(zone, rule)?;
        }
    }
    if let Some((zone, interface)) = &owned.tunnel {
        if DbusFirewall::interface_zone(interface)? == *zone {
            DbusFirewall::unbind_interface(zone, interface)?;
        }
    }
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}
impl Forwarding {
    /// Resolves zones before any mutation; fails if a service CIDR is unroutable.
    pub fn plan(settings: &Settings) -> io::Result<Self> {
        let network = settings
            .network
            .as_ref()
            .ok_or_else(|| io::Error::other("forwarding requires service CIDRs"))?;
        let mut rules = Vec::new();
        if settings.firewalld.masquerade {
            for cidr in &network.service_cidrs {
                let zone = DbusFirewall.zone(&egress_interface(cidr)?)?;
                rules.push((zone, format!(
                    "rule family=\"ipv4\" source address=\"{}\" destination address=\"{cidr}\" masquerade",
                    network.tunnel_pool
                )));
            }
        }
        Ok(Self {
            path: journal(settings),
            tunnel: (
                settings.firewalld.tunnel_zone.clone(),
                settings.interface.clone(),
            ),
            rules,
            owned: Mutex::new(Owned::default()),
        })
    }
    /// Records intent before each mutation so a crash leaves cleanup journaled.
    pub fn apply(&self) -> io::Result<()> {
        let mut owned = self.owned.lock().unwrap();
        let (zone, interface) = &self.tunnel;
        if DbusFirewall::interface_zone(interface)? != *zone {
            owned.tunnel = Some(self.tunnel.clone());
            save(&self.path, &owned)?;
            DbusFirewall::bind_interface(zone, interface)?;
        }
        for (zone, rule) in &self.rules {
            // An identical administrator rule is left alone and never removed.
            if !DbusFirewall.query(zone, rule)? {
                owned.rules.push((zone.clone(), rule.clone()));
                save(&self.path, &owned)?;
                DbusFirewall.add(zone, rule)?;
            }
        }
        Ok(())
    }
    /// Re-adds owned state that a firewalld reload discarded.
    pub fn restore(&self) -> io::Result<()> {
        let owned = self.owned.lock().unwrap();
        if let Some((zone, interface)) = &owned.tunnel {
            if DbusFirewall::interface_zone(interface)? != *zone {
                DbusFirewall::bind_interface(zone, interface)?;
            }
        }
        for (zone, rule) in &owned.rules {
            if !DbusFirewall.query(zone, rule)? {
                DbusFirewall.add(zone, rule)?;
            }
        }
        Ok(())
    }
    pub fn release(&self) -> io::Result<()> {
        let mut owned = self.owned.lock().unwrap();
        remove(&self.path, &owned)?;
        *owned = Owned::default();
        Ok(())
    }
}
/// Removes journaled state left by a dead process.
pub fn cleanup(settings: &Settings) -> io::Result<()> {
    let path = journal(settings);
    remove(&path, &load(&path)?)
}
