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
    #[serde(default = "keepalive")]
    pub keepalive_seconds: u64,
    /// A1 requires explicit administrator-provisioned admission. A2 adds managed firewalld.
    pub admission: AdmissionMode,
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
            Role::Client { entry, .. } if entry.port() == 0 => {
                return Err(io::Error::other("entry port must be nonzero"))
            }
            Role::Endpoint {
                listen,
                rotation_low,
                rotation_high,
                rotation_seconds,
            } => {
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
