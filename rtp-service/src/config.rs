use serde::{Deserialize, Serialize};
use std::{
    io,
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum Role {
    Client {
        entry: SocketAddrV4,
        #[serde(default = "localhost_any")]
        bind: SocketAddrV4,
    },
    Endpoint {
        listen: SocketAddrV4,
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
            "profile":"fixed", "key_file":"/tmp/key", "journal":"/tmp/fixed.json",
            "interface":"rtp0", "address":"10.88.0.1/24",
            "transport":{"role":"endpoint", "listen":"192.0.2.2:30000",
                "rotation_low":30001, "rotation_high":30008, "rotation_seconds":0},
            "initial_rotation_port":30001, "admission":"administrator_managed"
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
fn localhost_any() -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)
}
fn rotation() -> u64 {
    300
}
fn keepalive() -> u64 {
    15
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionMode {
    AdministratorManaged,
    Firewalld { agent_socket: PathBuf },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub profile: String,
    pub key_file: PathBuf,
    pub journal: PathBuf,
    pub interface: String,
    /// Required for clients: every outer socket is bound to this host device.
    pub outbound_interface: Option<String>,
    pub address: String,
    pub transport: Role,
    /// Optional deterministic initial data port; the relay adapter owns this
    /// RTP-specific choice. Scheduled rotations still use the configured pool.
    #[serde(default)]
    pub initial_rotation_port: Option<u16>,
    #[serde(default)]
    pub network: Option<crate::network::Network>,
    #[serde(default = "keepalive")]
    pub keepalive_seconds: u64,
    /// Explicit external admission or a policy-restricted local firewalld agent.
    pub admission: AdmissionMode,
    #[serde(default)]
    pub relay: Option<crate::relay::RelayConfig>,
}
impl Settings {
    pub fn load(path: &std::path::Path) -> io::Result<Self> {
        let value: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        value.validate()?;
        Ok(value)
    }
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
        if let Some(relay) = &self.relay {
            if !matches!(self.transport, Role::Endpoint { .. })
                || relay.management.owner != self.profile
                || !relay.listen_ip.is_ipv4()
                || !relay.endpoint_ip.is_ipv4()
            {
                return Err(io::Error::other(
                    "relay requires endpoint and same profile owner with IPv4 hops",
                ));
            }
        }
        if let Some(network) = &self.network {
            network.validate(self)?;
        }
        if let AdmissionMode::Firewalld { agent_socket } = &self.admission {
            if !agent_socket.is_absolute() {
                return Err(io::Error::other("agent_socket must be absolute"));
            }
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
        if !self.key_file.is_absolute() || !self.journal.is_absolute() {
            return Err(io::Error::other("key_file and journal must be absolute"));
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
