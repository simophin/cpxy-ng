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
        rotation_low: u16,
        rotation_high: u16,
        #[serde(default = "rotation")]
        rotation_seconds: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_data_port_is_endpoint_owned_and_inside_its_pool() {
        let mut settings: Settings = serde_json::from_value(serde_json::json!({
            "profile":"fixed", "journal":"/tmp/fixed.json",
            "interface":"rtp0", "address":"10.88.0.1/24",
            "transport":{"role":"endpoint", "listen":"192.0.2.2:30000",
                "rotation_low":30001, "rotation_high":30008, "rotation_seconds":0},
            "initial_rotation_port":30001, "firewall":"none"
        }))
        .unwrap();
        settings.validate().unwrap();
        for invalid in [0, 30000, 30009] {
            settings.initial_rotation_port = Some(invalid);
            assert!(settings.validate().is_err());
        }
        settings.initial_rotation_port = Some(30001);
        settings.transport = Role::Client {
            entry: "192.0.2.2:30000".parse().unwrap(),
            bind: "0.0.0.0:0".parse().unwrap(),
        };
        settings.outbound_interface = Some("eth0".into());
        assert!(settings.validate().is_err());
        settings.initial_rotation_port = None;
        settings.validate().unwrap();
    }
}
fn localhost_any() -> SocketAddr {
    SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)
}
fn rotation() -> u64 {
    300
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
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub profile: String,
    pub journal: PathBuf,
    pub interface: String,
    /// Required for clients: every outer socket is bound to this host device.
    pub outbound_interface: Option<String>,
    pub address: String,
    pub transport: Role,
    /// Optional deterministic initial data port inside the rotation pool.
    /// Scheduled rotations still use the configured pool.
    #[serde(default)]
    pub initial_rotation_port: Option<u16>,
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
            Role::Client { .. } if self.initial_rotation_port.is_some() => {
                return Err(io::Error::other("initial_rotation_port is endpoint-only"));
            }
            Role::Client { entry, .. } if entry.port() == 0 => {
                return Err(io::Error::other("entry port must be nonzero"))
            }
            Role::Endpoint {
                listen,
                rotation_low,
                rotation_high,
                rotation_seconds,
            } => {
                if self.initial_rotation_port.is_some_and(|port| {
                    port == listen.port() || !(rotation_low..=rotation_high).contains(&port)
                }) {
                    return Err(io::Error::other(
                        "initial_rotation_port must be a non-base port in the rotation pool",
                    ));
                }
                if listen.port() == 0
                    || rotation_low == 0
                    || rotation_high < rotation_low
                    || u32::from(rotation_high) - u32::from(rotation_low) + 1
                        - u32::from((rotation_low..=rotation_high).contains(&listen.port()))
                        < 3
                {
                    return Err(io::Error::other(
                        "endpoint needs base and three non-base rotation ports",
                    ));
                }
                if rotation_seconds == 1 || rotation_seconds > u64::MAX / 3 {
                    return Err(io::Error::other("invalid rotation_seconds"));
                }
            }
            _ => {}
        }
        Ok(())
    }
}
