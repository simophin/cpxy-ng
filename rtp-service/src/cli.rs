//! Command-line and environment interface. Every flag also reads a
//! `CPXY_RTP_*` variable; the shared password is read from the environment only.
use crate::{
    config::{FirewallMode, Firewalld, Role, Settings},
    network::{Dns, DnsMode, Network},
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
};

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
    /// Connect to an endpoint.
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
    /// Provision the same identity on both peers; bound into AEAD context.
    #[arg(long, env = "CPXY_RTP_SERVER_IDENTITY", default_value = "cpxy-rtp")]
    pub server_identity: String,
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
    /// Enable a full IPv4 exit. Client routing/firewall/NAT is externally managed;
    /// no host default route or policy rule is installed by the client.
    #[arg(long, env = "CPXY_RTP_INTERNET_EXIT", conflicts_with = "service_cidrs")]
    pub internet_exit: bool,
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
    #[arg(long, env = "CPXY_RTP_POOL_LOW")]
    pub pool_low: Option<Ipv4Addr>,
    #[arg(long, env = "CPXY_RTP_POOL_HIGH")]
    pub pool_high: Option<Ipv4Addr>,
    #[arg(long = "reserved-address", value_delimiter = ',')]
    pub reserved_addresses: Vec<Ipv4Addr>,
    #[command(flatten)]
    pub common: Common,
    /// Plain HTTP control listen address and TCP port.
    #[arg(long, env = "CPXY_RTP_LISTEN")]
    pub listen: SocketAddr,
    /// Dedicated UDP allocation pool LOW-HIGH; defaults to the next 8 ports.
    #[arg(long, env = "CPXY_RTP_UDP_PORTS", value_parser = port_range)]
    pub udp_ports: Option<(u16, u16)>,
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
        value_delimiter = ','
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
}
#[derive(Args, Debug, Clone)]
pub struct ClientArgs {
    /// Obtain a persistent UUID/address assignment before configuring the TUN.
    #[arg(long, env = "CPXY_RTP_DYNAMIC_ADDRESS")]
    pub dynamic_address: bool,
    #[command(flatten)]
    pub common: Common,
    /// Endpoint HTTP control IP address and TCP port.
    #[arg(long, env = "CPXY_RTP_ENTRY")]
    pub entry: SocketAddr,
    /// Local UDP bind address; defaults to the entry's address family.
    #[arg(long, env = "CPXY_RTP_BIND")]
    pub bind: Option<SocketAddr>,
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
    if n.service_cidrs.is_empty() && !n.internet_exit {
        return Ok(None);
    }
    Ok(Some(Network {
        internet_exit: n.internet_exit,
        tunnel_pool: pool(address)?,
        client_addresses: clients,
        service_cidrs: if n.internet_exit {
            vec!["0.0.0.0/0".into()]
        } else {
            n.service_cidrs.clone()
        },
        table: n.route_table,
        rule_priority: n.rule_priority,
        dns: n.dns_resolver.map(|resolver| Dns {
            mode: DnsMode::ExternallyManagedFailClosed,
            resolver,
            suffixes: n.dns_suffixes.clone(),
        }),
    }))
}
/// A fixed identity labels leases and firewalld rules for each instance.
pub const NAME: &str = "cpxy-rtp";
fn journal(common: &Common) -> PathBuf {
    common.state_dir.join("state.json")
}
impl EndpointArgs {
    pub fn settings(&self) -> io::Result<Settings> {
        let c = &self.common;
        let (udp_low, udp_high) = self.udp_ports.unwrap_or_else(|| {
            let base = self.listen.port();
            (base.saturating_add(1), base.saturating_add(8))
        });
        let settings = Settings {
            control: crate::config::ControlSettings {
                server_identity: c.server_identity.clone(),
                pool_low: self.pool_low,
                pool_high: self.pool_high,
                reserved: self.reserved_addresses.clone(),
                dynamic_address: false,
            },
            profile: NAME.into(),
            journal: journal(c),
            interface: c.interface.clone(),
            outbound_interface: None,
            address: self.address.clone(),
            transport: Role::Endpoint {
                listen: self.listen,
                udp_low,
                udp_high,
            },
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
                sources: if self.firewalld_sources.is_empty() {
                    if self.listen.is_ipv6() && self.listen.ip().is_unspecified() {
                        vec!["0.0.0.0/0".into(), "::/0".into()]
                    } else {
                        vec![if self.listen.is_ipv4() {
                            "0.0.0.0/0"
                        } else {
                            "::/0"
                        }
                        .into()]
                    }
                } else {
                    self.firewalld_sources.clone()
                },
                tunnel_zone: self.firewalld_tunnel_zone.clone(),
                masquerade: !self.no_masquerade,
            },
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
            control: crate::config::ControlSettings {
                server_identity: c.server_identity.clone(),
                dynamic_address: self.dynamic_address,
                ..Default::default()
            },
            profile: NAME.into(),
            journal: journal(c),
            interface: c.interface.clone(),
            outbound_interface: Some(self.outbound_interface.clone()),
            address: self.address.clone(),
            transport: Role::Client {
                entry: self.entry,
                bind: self.bind.unwrap_or_else(|| {
                    SocketAddr::new(
                        if self.entry.is_ipv4() {
                            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
                        } else {
                            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
                        },
                        0,
                    )
                }),
            },
            network: network(c, &self.address, vec![own])?,
            keepalive_seconds: c.keepalive_seconds,
            firewall: FirewallMode::None,
            firewalld: Firewalld::default(),
        };
        settings.validate()?;
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ipv6_transport_defaults_and_dual_stack_admission() {
        let Cli {
            role: RoleCommand::Client { args, .. },
        } = parse(&[
            "client",
            "--entry",
            "[2001:db8::1]:30000",
            "--outbound-interface",
            "eth0",
        ])
        else {
            panic!()
        };
        let settings = args.settings().unwrap();
        assert_eq!(settings.inner().unwrap().0, Ipv4Addr::new(10, 88, 0, 2));
        let Role::Client { bind, .. } = settings.transport else {
            panic!()
        };
        assert_eq!(bind, "[::]:0".parse().unwrap());
        let mut mismatched = args.clone();
        mismatched.bind = Some("0.0.0.0:0".parse().unwrap());
        assert!(mismatched.settings().is_err());
        for (listen, sources) in [
            ("[::]:30000", vec!["0.0.0.0/0", "::/0"]),
            ("[2001:db8::1]:30000", vec!["::/0"]),
            ("192.0.2.1:30000", vec!["0.0.0.0/0"]),
        ] {
            let Cli {
                role: RoleCommand::Endpoint { args, .. },
            } = parse(&["endpoint", "--listen", listen])
            else {
                panic!()
            };
            assert_eq!(args.settings().unwrap().firewalld.sources, sources);
        }
    }
    #[test]
    fn internet_exit_is_explicit_and_excludes_service_selection() {
        let Cli {
            role: RoleCommand::Client { args, .. },
        } = parse(&[
            "client",
            "--entry",
            "192.0.2.2:30000",
            "--outbound-interface",
            "eth0",
            "--internet-exit",
        ])
        else {
            panic!()
        };
        let settings = args.settings().unwrap();
        let n = settings.network.unwrap();
        assert!(n.internet_exit);
        assert_eq!(n.service_cidrs, ["0.0.0.0/0"]);
        assert!(Cli::try_parse_from([
            "cpxy-rtp",
            "client",
            "--entry",
            "192.0.2.2:30000",
            "--outbound-interface",
            "eth0",
            "--internet-exit",
            "--service-cidr",
            "10.90.0.0/24",
        ])
        .is_err());
        let Cli {
            role: RoleCommand::Client { args, .. },
        } = parse(&[
            "client",
            "--entry",
            "192.0.2.2:30000",
            "--outbound-interface",
            "eth0",
            "--service-cidr",
            "0.0.0.0/0",
        ])
        else {
            panic!()
        };
        assert!(args.settings().is_err());
    }
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
                udp_low: 30001,
                udp_high: 30008,
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
    fn removed_relay_management_flag_is_rejected() {
        let error = Cli::try_parse_from([
            "cpxy-rtp",
            "endpoint",
            "--listen",
            "192.0.2.2:30000",
            "--relay-management",
            "192.0.2.9:7000",
        ])
        .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }
}
