//! Command-line and environment interface. Every flag also reads a
//! `CPXY_RTP_*` variable; the shared password is read from the environment only.
use crate::{
    config::{FirewallMode, Firewalld, Role, Settings},
    network::{Dns, DnsMode, Network},
    relay::RelayConfig,
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    path::PathBuf,
};
use udp_relay::management::{ClientConfig, Tls};

#[derive(Parser, Debug)]
#[command(
    name = "cpxy-rtp",
    about = "RTP-framed IPv4 tunnel endpoint and client",
    after_help = "The shared password is read only from CPXY_RTP_PASSWORD (at least 12 characters)."
)]
pub struct Cli {
    #[command(subcommand)]
    pub role: RoleCommand,
}
#[derive(Subcommand, Debug)]
pub enum RoleCommand {
    /// Accept clients and forward their traffic to the selected service CIDRs.
    Endpoint {
        #[command(flatten)]
        args: EndpointArgs,
        #[command(subcommand)]
        action: Option<Action>,
    },
    /// Connect to an endpoint or relay entry.
    Client {
        #[command(flatten)]
        args: ClientArgs,
        #[command(subcommand)]
        action: Option<ClientAction>,
    },
    /// Internal: the unprivileged namespace half of `client namespace`.
    #[command(name = "namespace-child", hide = true)]
    NamespaceChild {
        #[arg(long)]
        fd: i32,
        #[arg(last = true)]
        command: Vec<String>,
    },
}
#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Run until SIGINT/SIGTERM (default).
    Run,
    /// Read-only diagnostics as JSON.
    Doctor,
    /// Release owned firewall rules and routing policy after a failure.
    Cleanup,
    /// Release owned firewall rules only, keeping fail-closed routing guards.
    CleanupPorts,
}
#[derive(Subcommand, Debug)]
pub enum ClientAction {
    #[command(flatten)]
    Common(Action),
    /// Run inside an unprivileged user/network namespace, then run COMMAND
    /// once connected (or until a signal when no COMMAND is given).
    Namespace {
        #[arg(last = true)]
        command: Vec<String>,
    },
}

#[derive(Args, Debug, Clone)]
pub struct Common {
    /// TUN interface name created and owned by the service.
    #[arg(long, env = "CPXY_RTP_INTERFACE", default_value = "rtp0")]
    pub interface: String,
    /// Directory for the lease and routing journals.
    #[arg(long, env = "CPXY_RTP_STATE_DIR", default_value = "/var/lib/cpxy-rtp")]
    pub state_dir: PathBuf,
    #[arg(long, env = "CPXY_RTP_KEEPALIVE_SECONDS", default_value_t = 15)]
    pub keepalive_seconds: u64,
    #[command(flatten)]
    pub network: NetworkArgs,
}
#[derive(Args, Debug, Clone)]
pub struct NetworkArgs {
    /// IPv4 CIDRs reachable through the tunnel; enables routing policy.
    #[arg(
        long = "service-cidr",
        env = "CPXY_RTP_SERVICE_CIDRS",
        value_delimiter = ','
    )]
    pub service_cidrs: Vec<String>,
    /// Reserved, otherwise empty routing table for the fail-closed guard.
    #[arg(long, env = "CPXY_RTP_ROUTE_TABLE", default_value_t = 18800)]
    pub route_table: u32,
    /// Reserved policy rule priority, earlier than other custom rules.
    #[arg(long, env = "CPXY_RTP_RULE_PRIORITY", default_value_t = 1000)]
    pub rule_priority: u32,
    /// Private resolver inside a service CIDR (metadata; DNS is externally managed).
    #[arg(long, env = "CPXY_RTP_DNS_RESOLVER", requires = "dns_suffixes")]
    pub dns_resolver: Option<Ipv4Addr>,
    #[arg(
        long = "dns-suffix",
        env = "CPXY_RTP_DNS_SUFFIXES",
        value_delimiter = ',',
        requires = "dns_resolver"
    )]
    pub dns_suffixes: Vec<String>,
}
#[derive(Args, Debug, Clone)]
pub struct EndpointArgs {
    #[command(flatten)]
    pub common: Common,
    /// Public IPv4 listen address and base port.
    #[arg(long, env = "CPXY_RTP_LISTEN")]
    pub listen: SocketAddrV4,
    /// Rotation port pool LOW-HIGH; defaults to the 8 ports after the base port.
    #[arg(long, env = "CPXY_RTP_ROTATION_PORTS", value_parser = port_range)]
    pub rotation_ports: Option<(u16, u16)>,
    /// Seconds between port rotations; 0 disables scheduled rotation.
    #[arg(long, env = "CPXY_RTP_ROTATION_SECONDS", default_value_t = 300)]
    pub rotation_seconds: u64,
    #[arg(long, env = "CPXY_RTP_INITIAL_ROTATION_PORT")]
    pub initial_rotation_port: Option<u16>,
    /// Tunnel address and prefix of this endpoint.
    #[arg(long, env = "CPXY_RTP_ADDRESS", default_value = "10.88.0.1/24")]
    pub address: String,
    /// Client tunnel addresses allowed to reach the service CIDRs.
    #[arg(
        long = "client-address",
        env = "CPXY_RTP_CLIENT_ADDRESSES",
        value_delimiter = ',',
        default_value = "10.88.0.2"
    )]
    pub client_addresses: Vec<Ipv4Addr>,
    /// `auto` manages firewalld when it answers on the system bus.
    #[arg(long, env = "CPXY_RTP_FIREWALL", value_enum, default_value_t = FirewallArg::Auto)]
    pub firewall: FirewallArg,
    #[arg(long, env = "CPXY_RTP_FIREWALLD_ZONE")]
    pub firewalld_zone: Option<String>,
    #[arg(long, env = "CPXY_RTP_FIREWALLD_INTERFACE")]
    pub firewalld_interface: Option<String>,
    /// Outer source CIDRs allowed to reach the UDP ports (1..4).
    #[arg(
        long = "firewalld-source",
        env = "CPXY_RTP_FIREWALLD_SOURCES",
        value_delimiter = ',',
        default_value = "0.0.0.0/0"
    )]
    pub firewalld_sources: Vec<String>,
    /// Runtime zone for the TUN so firewalld forwards decrypted packets.
    #[arg(
        long,
        env = "CPXY_RTP_FIREWALLD_TUNNEL_ZONE",
        default_value = "trusted"
    )]
    pub firewalld_tunnel_zone: String,
    /// Keep client source addresses; backends then need a return route to the tunnel pool.
    #[arg(long, env = "CPXY_RTP_NO_MASQUERADE")]
    pub no_masquerade: bool,
    #[command(flatten)]
    pub relay: RelayArgs,
}
#[derive(Args, Debug, Clone)]
pub struct RelayArgs {
    /// Relay management address; enables coordinated relay port leases.
    #[arg(long, env = "CPXY_RTP_RELAY_MANAGEMENT",
        requires_all = ["relay_server_name", "relay_ca", "relay_certificate", "relay_private_key", "relay_listen_ip", "relay_endpoint_ip"])]
    pub relay_management: Option<SocketAddr>,
    #[arg(long, env = "CPXY_RTP_RELAY_SERVER_NAME")]
    pub relay_server_name: Option<String>,
    #[arg(long, env = "CPXY_RTP_RELAY_CA")]
    pub relay_ca: Option<PathBuf>,
    #[arg(long, env = "CPXY_RTP_RELAY_CERTIFICATE")]
    pub relay_certificate: Option<PathBuf>,
    #[arg(long, env = "CPXY_RTP_RELAY_PRIVATE_KEY")]
    pub relay_private_key: Option<PathBuf>,
    /// Relay public IPv4 that clients use as their entry.
    #[arg(long, env = "CPXY_RTP_RELAY_LISTEN_IP")]
    pub relay_listen_ip: Option<Ipv4Addr>,
    /// This endpoint's public IPv4 as seen by the relay.
    #[arg(long, env = "CPXY_RTP_RELAY_ENDPOINT_IP")]
    pub relay_endpoint_ip: Option<Ipv4Addr>,
}
#[derive(Args, Debug, Clone)]
pub struct ClientArgs {
    #[command(flatten)]
    pub common: Common,
    /// Endpoint or relay entry: public IPv4 and base port.
    #[arg(long, env = "CPXY_RTP_ENTRY")]
    pub entry: SocketAddrV4,
    #[arg(long, env = "CPXY_RTP_BIND", default_value = "0.0.0.0:0")]
    pub bind: SocketAddrV4,
    /// Physical interface every outer socket is bound to (bypasses the TUN).
    #[arg(long, env = "CPXY_RTP_OUTBOUND_INTERFACE")]
    pub outbound_interface: String,
    /// Tunnel address and prefix of this client.
    #[arg(long, env = "CPXY_RTP_ADDRESS", default_value = "10.88.0.2/24")]
    pub address: String,
}
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirewallArg {
    Auto,
    Firewalld,
    None,
}

fn port_range(s: &str) -> Result<(u16, u16), String> {
    let (low, high) = s.split_once('-').ok_or("expected LOW-HIGH")?;
    let low = low.parse().map_err(|_| "invalid low port")?;
    let high = high.parse().map_err(|_| "invalid high port")?;
    Ok((low, high))
}
/// Canonical network containing an `IPv4/prefix` address.
fn pool(address: &str) -> io::Result<String> {
    let (ip, prefix) = address
        .split_once('/')
        .ok_or_else(|| io::Error::other("address needs IPv4/prefix"))?;
    let ip: Ipv4Addr = ip
        .parse()
        .map_err(|_| io::Error::other("invalid IPv4 address"))?;
    let prefix: u8 = prefix
        .parse()
        .map_err(|_| io::Error::other("invalid prefix"))?;
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
    };
    Ok(format!("{}/{prefix}", Ipv4Addr::from(u32::from(ip) & mask)))
}
fn network(common: &Common, address: &str, clients: Vec<Ipv4Addr>) -> io::Result<Option<Network>> {
    let n = &common.network;
    if n.service_cidrs.is_empty() {
        return Ok(None);
    }
    Ok(Some(Network {
        tunnel_pool: pool(address)?,
        client_addresses: clients,
        service_cidrs: n.service_cidrs.clone(),
        table: n.route_table,
        rule_priority: n.rule_priority,
        dns: n.dns_resolver.map(|resolver| Dns {
            mode: DnsMode::ExternallyManagedFailClosed,
            resolver,
            suffixes: n.dns_suffixes.clone(),
        }),
    }))
}
/// One instance per host: a fixed identity labels leases, firewalld rules and
/// the relay management owner (the relay's `authority.owner` must match).
pub const NAME: &str = "cpxy-rtp";
fn journal(common: &Common) -> PathBuf {
    common.state_dir.join("state.json")
}
impl EndpointArgs {
    pub fn settings(&self) -> io::Result<Settings> {
        let c = &self.common;
        let (rotation_low, rotation_high) = self.rotation_ports.unwrap_or_else(|| {
            let base = self.listen.port();
            (base.saturating_add(1), base.saturating_add(8))
        });
        let r = &self.relay;
        let relay = match r.relay_management {
            None => None,
            Some(address) => Some(RelayConfig {
                management: ClientConfig {
                    address,
                    server_name: r.relay_server_name.clone().unwrap_or_default(),
                    tls: Tls {
                        ca: r.relay_ca.clone().unwrap_or_default(),
                        certificate: r.relay_certificate.clone().unwrap_or_default(),
                        private_key: r.relay_private_key.clone().unwrap_or_default(),
                    },
                    owner: NAME.into(),
                },
                listen_ip: IpAddr::V4(r.relay_listen_ip.unwrap_or(Ipv4Addr::UNSPECIFIED)),
                endpoint_ip: IpAddr::V4(r.relay_endpoint_ip.unwrap_or(Ipv4Addr::UNSPECIFIED)),
            }),
        };
        let settings = Settings {
            profile: NAME.into(),
            journal: journal(c),
            interface: c.interface.clone(),
            outbound_interface: None,
            address: self.address.clone(),
            transport: Role::Endpoint {
                listen: self.listen,
                rotation_low,
                rotation_high,
                rotation_seconds: self.rotation_seconds,
            },
            initial_rotation_port: self.initial_rotation_port,
            network: network(c, &self.address, self.client_addresses.clone())?,
            keepalive_seconds: c.keepalive_seconds,
            firewall: match self.firewall {
                FirewallArg::Auto => FirewallMode::Auto,
                FirewallArg::Firewalld => FirewallMode::Firewalld,
                FirewallArg::None => FirewallMode::None,
            },
            firewalld: Firewalld {
                zone: self.firewalld_zone.clone(),
                interface: self.firewalld_interface.clone(),
                sources: self.firewalld_sources.clone(),
                tunnel_zone: self.firewalld_tunnel_zone.clone(),
                masquerade: !self.no_masquerade,
            },
            relay,
        };
        settings.validate()?;
        Ok(settings)
    }
}
impl ClientArgs {
    pub fn settings(&self) -> io::Result<Settings> {
        let c = &self.common;
        pool(&self.address)?;
        let own: Ipv4Addr = self
            .address
            .split('/')
            .next()
            .unwrap_or_default()
            .parse()
            .map_err(|_| io::Error::other("invalid IPv4 address"))?;
        let settings = Settings {
            profile: NAME.into(),
            journal: journal(c),
            interface: c.interface.clone(),
            outbound_interface: Some(self.outbound_interface.clone()),
            address: self.address.clone(),
            transport: Role::Client {
                entry: self.entry,
                bind: self.bind,
            },
            initial_rotation_port: None,
            network: network(c, &self.address, vec![own])?,
            keepalive_seconds: c.keepalive_seconds,
            firewall: FirewallMode::None,
            firewalld: Firewalld::default(),
            relay: None,
        };
        settings.validate()?;
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("cpxy-rtp").chain(args.iter().copied())).unwrap()
    }
    #[test]
    fn endpoint_defaults_derive_rotation_pool_journal_and_network() {
        let Cli {
            role: RoleCommand::Endpoint { args, action },
        } = parse(&[
            "endpoint",
            "--listen",
            "192.0.2.2:30000",
            "--state-dir",
            "/var/lib/x",
            "--service-cidr",
            "10.90.0.0/24,10.91.0.0/24",
            "doctor",
        ])
        else {
            panic!()
        };
        assert_eq!(action, Some(Action::Doctor));
        let s = args.settings().unwrap();
        assert!(matches!(
            s.transport,
            Role::Endpoint {
                rotation_low: 30001,
                rotation_high: 30008,
                rotation_seconds: 300,
                ..
            }
        ));
        assert_eq!(s.journal, PathBuf::from("/var/lib/x/state.json"));
        assert_eq!(s.profile, NAME);
        assert_eq!(s.firewall, FirewallMode::Auto);
        let n = s.network.unwrap();
        assert_eq!(n.tunnel_pool, "10.88.0.0/24");
        assert_eq!(n.client_addresses, vec![Ipv4Addr::new(10, 88, 0, 2)]);
        assert_eq!(n.service_cidrs.len(), 2);
    }
    #[test]
    fn client_requires_entry_and_outbound_interface_and_owns_its_address() {
        assert!(Cli::try_parse_from(["cpxy-rtp", "client", "--entry", "192.0.2.2:30000"]).is_err());
        let Cli {
            role: RoleCommand::Client { args, action },
        } = parse(&[
            "client",
            "--entry",
            "192.0.2.2:30000",
            "--outbound-interface",
            "eth0",
            "--service-cidr",
            "10.90.0.0/24",
            "namespace",
            "--",
            "curl",
            "http://10.90.0.2/",
        ])
        else {
            panic!()
        };
        assert!(
            matches!(action, Some(ClientAction::Namespace { ref command }) if command == &["curl", "http://10.90.0.2/"])
        );
        let s = args.settings().unwrap();
        assert_eq!(s.firewall, FirewallMode::None);
        assert_eq!(
            s.network.unwrap().client_addresses,
            vec![Ipv4Addr::new(10, 88, 0, 2)]
        );
    }
    #[test]
    fn relay_flags_are_all_or_nothing() {
        assert!(Cli::try_parse_from([
            "cpxy-rtp",
            "endpoint",
            "--listen",
            "192.0.2.2:30000",
            "--relay-management",
            "192.0.2.9:7000",
        ])
        .is_err());
    }
}
