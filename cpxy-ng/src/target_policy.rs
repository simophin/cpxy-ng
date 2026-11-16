//! Which addresses a cpxy server or reverse exit connects to on a client's behalf.
//!
//! Anyone with the key chooses the target, so by default the server and the reverse exit refuse
//! internal addresses: loopback, private, link-local, CGNAT (which includes Tailscale), and
//! special-purpose ranges. Otherwise the key would open the services on their own machine and
//! network. The check applies to the resolved addresses, and only addresses that pass it are
//! connected to, so a name that resolves (or later re-resolves) to an internal address can't get
//! through. `--allow-target` opens specific ranges again.

use anyhow::{Context, format_err};
use ipnet::IpNet;
use std::fmt::{Display, Formatter};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;
use tokio::net::TcpStream;

/// The addresses connections may go to.
#[derive(Debug, Clone, Default)]
pub struct TargetPolicy {
    allowed: Vec<IpNet>,
}

/// A target refused because every address it has is internal.
#[derive(Debug)]
pub struct Refused {
    host: String,
    addresses: Vec<IpAddr>,
}

impl Display for Refused {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let addresses: Vec<_> = self.addresses.iter().map(IpAddr::to_string).collect();
        write!(
            f,
            "Refusing to connect to {}: it has only internal addresses ({})",
            self.host,
            addresses.join(", ")
        )
    }
}

impl std::error::Error for Refused {}

impl TargetPolicy {
    /// Refuses internal addresses except those in `allowed`.
    pub fn new(allowed: Vec<IpNet>) -> Self {
        Self { allowed }
    }

    /// Parses an `--allow-target` value: a CIDR, or a single address.
    pub fn parse_allowed(s: &str) -> Result<IpNet, String> {
        IpNet::from_str(s)
            .or_else(|_| IpAddr::from_str(s).map(IpNet::from))
            .map_err(|_| format!("{s:?} is neither a CIDR nor an IP address"))
    }

    pub fn permits(&self, ip: IpAddr) -> bool {
        !is_internal(ip) || self.allowed.iter().any(|net| net.contains(&ip))
    }

    /// Resolves `host` and connects to the first of its permitted addresses that accepts, with
    /// `TCP_NODELAY` set. Fails with [`Refused`] when none is permitted.
    pub async fn connect(&self, host: &str, port: u16) -> anyhow::Result<TcpStream> {
        let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .with_context(|| format!("Error resolving {host}"))?
            .collect();
        let (permitted, refused): (Vec<_>, Vec<_>) =
            resolved.into_iter().partition(|a| self.permits(a.ip()));
        if permitted.is_empty() {
            return Err(Refused {
                host: host.to_string(),
                addresses: refused.iter().map(SocketAddr::ip).collect(),
            }
            .into());
        }

        let mut last_error = None;
        for address in permitted {
            match TcpStream::connect(address).await {
                Ok(stream) => {
                    stream.set_nodelay(true).context("Error setting nodelay")?;
                    return Ok(stream);
                }
                Err(e) => last_error = Some(e),
            }
        }
        Err(last_error
            .map(anyhow::Error::from)
            .unwrap_or_else(|| format_err!("No address to connect to")))
        .with_context(|| format!("Error connecting to {host}:{port}"))
    }
}

/// Addresses that belong to the machine itself or a private network, rather than the internet.
pub fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_internal_v4(ip),
        IpAddr::V6(ip) => is_internal_v6(ip),
    }
}

fn is_internal_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_unspecified()
        || a == 0 // "this network"
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || (a == 100 && (b & 0xc0) == 64) // CGNAT 100.64.0.0/10, used by Tailscale
        || ip.is_multicast()
        || a >= 240 // reserved, and broadcast
}

fn is_internal_v6(ip: Ipv6Addr) -> bool {
    // IPv4 addresses written as IPv6, which reach the IPv4 address: mapped (::ffff:a.b.c.d) and
    // NAT64 (64:ff9b::a.b.c.d).
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_internal_v4(v4);
    }
    let segments = ip.segments();
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [.., hi, lo] = segments;
        return is_internal_v4(Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo)));
    }
    ip.is_unspecified()
        || ip.is_loopback()
        || (segments[0] & 0xfe00) == 0xfc00 // unique local fc00::/7, includes Tailscale's
        || (segments[0] & 0xffc0) == 0xfe80 // link-local
        || (segments[0] & 0xffc0) == 0xfec0 // site-local (deprecated)
        || ip.is_multicast()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn classifies_addresses() {
        for internal in [
            "0.0.0.0",
            "0.1.2.3",
            "127.0.0.1",
            "127.8.9.10",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.79.255.37",
            "100.127.255.255",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fd7a:115c:a1e0::1",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.0.1",
            "64:ff9b::7f00:1",
            "64:ff9b::a00:1",
        ] {
            assert!(is_internal(internal.parse().unwrap()), "{internal}");
        }

        for public in [
            "1.1.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
            "172.15.0.1",
            "172.32.0.1",
            "223.5.5.5",
            "2001:4860:4860::8888",
            "2400:3200::1",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ] {
            assert!(!is_internal(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn allowed_ranges_are_permitted() {
        let policy = TargetPolicy::new(vec![
            TargetPolicy::parse_allowed("10.0.0.0/8").unwrap(),
            TargetPolicy::parse_allowed("::1").unwrap(),
        ]);
        assert!(policy.permits("10.9.8.7".parse().unwrap()));
        assert!(policy.permits("::1".parse().unwrap()));
        assert!(policy.permits("8.8.8.8".parse().unwrap()));
        assert!(!policy.permits("127.0.0.1".parse().unwrap()));
        assert!(!policy.permits("192.168.0.1".parse().unwrap()));
        assert!(TargetPolicy::parse_allowed("not an address").is_err());
    }

    #[tokio::test]
    async fn refuses_internal_targets_by_default() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        for host in ["127.0.0.1", "localhost"] {
            let err = TargetPolicy::default()
                .connect(host, port)
                .await
                .unwrap_err();
            assert!(err.downcast_ref::<Refused>().is_some(), "{host}: {err:#}");
        }
    }

    #[tokio::test]
    async fn connects_to_allowed_targets() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let policy = TargetPolicy::new(vec!["127.0.0.0/8".parse().unwrap()]);
        policy.connect("127.0.0.1", port).await.unwrap();
    }
}
