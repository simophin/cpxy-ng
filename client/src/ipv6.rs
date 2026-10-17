use serde::{Deserialize, Serialize};

/// How much IPv6 the client relays. Proxied IPv6 is sent to the cpxy server as an IPv6 address,
/// so only [`Ipv6Mode::Full`] needs a server that can reach IPv6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Ipv6Mode {
    /// IPv6 is refused and AAAA queries get no records, so apps use IPv4.
    Off,
    /// IPv6 is relayed only to addresses that route direct, and DNS hands out only their AAAA
    /// records. For a cpxy server without IPv6.
    Direct,
    /// All IPv6 is relayed, direct or through the cpxy server.
    #[default]
    Full,
}
