use serde::{Deserialize, Serialize};
use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum Role {
    Client {
        entry: SocketAddr,
        #[serde(default = "localhost_any")]
        bind: SocketAddr,
    },
    Endpoint {
        listen: SocketAddr,
        /// Both zero means let the OS assign a port with bind(:0).
        #[serde(default)]
        udp_low: u16,
        #[serde(default)]
        udp_high: u16,
    },
}

fn localhost_any() -> SocketAddr {
    SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)
}
fn keepalive() -> u64 {
    15
}
/// Endpoint UDP admission. `Auto` uses firewalld when it answers on the system
/// bus and otherwise leaves port policy to the administrator.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FirewallMode {
    #[default]
    Auto,
    Firewalld,
    None,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Firewalld {
    /// Zone for the UDP rich rules; defaults to the zone of `interface`.
    pub zone: Option<String>,
    /// Public interface carrying the listener; defaults to the one owning
    /// the listen address, or the default-route interface for 0.0.0.0.
    pub interface: Option<String>,
    /// Allowed outer source CIDRs (1..4).
    pub sources: Vec<String>,
    /// Runtime zone for the service's own TUN so decrypted packets can be
    /// forwarded; the packet allowlist still limits them to service CIDRs.
    pub tunnel_zone: String,
    /// Masquerade tunnel-pool traffic to each service CIDR so backends need no
    /// return route; they then see the endpoint's address instead of clients'.
    pub masquerade: bool,
}
impl Default for Firewalld {
    fn default() -> Self {
        Self {
            zone: None,
            interface: None,
            sources: vec!["0.0.0.0/0".into()],
            tunnel_zone: "trusted".into(),
            masquerade: true,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ControlSettings {
    pub server_identity: String,
    pub pool_low: Option<Ipv4Addr>,
    pub pool_high: Option<Ipv4Addr>,
    pub reserved: Vec<Ipv4Addr>,
    pub dynamic_address: bool,
}
impl Default for ControlSettings {
    fn default() -> Self {
        Self {
            server_identity: "cpxy-rtp".into(),
            pool_low: None,
            pool_high: None,
            reserved: Vec::new(),
            dynamic_address: false,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub control: ControlSettings,
    pub profile: String,
    pub journal: PathBuf,
    pub interface: String,
    /// Required for clients: every outer socket is bound to this host device.
    pub outbound_interface: Option<String>,
    pub address: String,
    pub transport: Role,
    #[serde(default)]
    pub network: Option<crate::network::Network>,
    #[serde(default = "keepalive")]
    pub keepalive_seconds: u64,
    #[serde(default)]
    pub firewall: FirewallMode,
    #[serde(default)]
    pub firewalld: Firewalld,
}
impl Settings {
    pub fn inner(&self) -> io::Result<(Ipv4Addr, u8)> {
        let (ip, prefix) = self
            .address
            .split_once('/')
            .ok_or_else(|| io::Error::other("address needs IPv4/prefix"))?;
        let ip = ip
            .parse()
            .map_err(|_| io::Error::other("invalid IPv4 address"))?;
        let prefix: u8 = prefix
            .parse()
            .map_err(|_| io::Error::other("invalid prefix"))?;
        if prefix > 32 {
            return Err(io::Error::other("invalid prefix"));
        }
        Ok((ip, prefix))
    }
    pub fn validate(&self) -> io::Result<()> {
        self.inner()?;
        if self.control.server_identity.is_empty() || self.control.server_identity.len() > 255 {
            return Err(io::Error::other("server identity must be 1..255 bytes"));
        }

        if let Some(network) = &self.network {
            network.validate(self)?;
        }
        if matches!(self.transport, Role::Client { .. }) && self.firewall != FirewallMode::None {
            return Err(io::Error::other(
                "clients own no listeners; firewall must be none",
            ));
        }
        if self.profile.is_empty()
            || !self
                .profile
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(io::Error::other("invalid profile name"));
        }
        if self.interface.is_empty()
            || self.interface.len() > 15
            || !self.interface.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(io::Error::other("invalid TUN name"));
        }
        if !self.journal.is_absolute() {
            return Err(io::Error::other("state directory must be absolute"));
        }
        if self.keepalive_seconds < 5 || self.keepalive_seconds > 45 {
            return Err(io::Error::other("keepalive_seconds must be 5..45"));
        }
        if matches!(self.transport, Role::Client { .. })
            && self.outbound_interface.as_ref().is_none_or(|name| {
                name.is_empty()
                    || name.len() > 15
                    || name == &self.interface
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
            })
        {
            return Err(io::Error::other(
                "client requires a physical outbound_interface",
            ));
        }
        if let Role::Client { entry, bind } = self.transport {
            if entry.is_ipv4() != bind.is_ipv4() {
                return Err(io::Error::other(
                    "entry and bind address families must match",
                ));
            }
        }
        match self.transport {
            Role::Client { entry, .. } if entry.port() == 0 => {
                return Err(io::Error::other("entry port must be nonzero"))
            }
            Role::Endpoint {
                listen,
                udp_low,
                udp_high,
            } => {
                if listen.port() == 0 || (udp_low == 0) != (udp_high == 0) || udp_high < udp_low {
                    return Err(io::Error::other(
                        "endpoint requires TCP control port and either automatic UDP ports or a valid nonzero range",
                    ));
                }
                if self.inner()?.1 > 30 || self.control.dynamic_address {
                    return Err(io::Error::other(
                        "endpoint requires a usable IPv4 subnet and static server address",
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }
}
