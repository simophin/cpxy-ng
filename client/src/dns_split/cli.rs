//! `cpxy dns`: a split DNS server.

use super::cache::{CacheOptions, DnsCache, purge_expired_every};
use super::is_local_region_ip;
use super::policy::Racer;
use super::routing::route_spec;
use super::server::{DnsSplitHandler, serve_tcp, serve_udp};
use super::spec::{ServerSpec, Via};
use super::upstream::{
    Dialer, DnsUpstream, Group, HickoryUpstream, OutboundDialer, tls_client_config,
};
use crate::ipv6::Ipv6Mode;
use crate::outbound::{ProtocolOutbound, ProxyRoutes, Socks5Outbound};
use crate::protocol_config::Config as ServerConfig;
use anyhow::Context;
use hickory_proto::rr::Name;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, UdpSocket};

const PURGE_INTERVAL: Duration = Duration::from_secs(300);

/// Where answers are cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cache {
    Memory,
    Off,
    File(PathBuf),
}

impl FromStr for Cache {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "memory" => Cache::Memory,
            "off" => Cache::Off,
            path => Cache::File(path.into()),
        })
    }
}

/// How the DNS servers themselves are reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ResolverRouting {
    /// Directly, except the servers given with `?via=cpxy`
    Explicit,
    /// By address: Chinese and private servers directly, the rest (and --proxy-route) through
    /// --server or --socks5-proxy, over TCP. The --local-server is always asked directly. For a
    /// router, whose own sockets bypass its firewall's routing.
    #[value(name = "cn_split")]
    CnSplit,
}

/// A DNS forwarder that races queries across upstream and alternative servers. An answer with any
/// address in the local region from an upstream wins, keeping just those addresses. Alternative
/// answers wait until upstreams finish without a local answer, or the query deadline expires.
#[derive(Debug, clap::Args)]
#[command(group = clap::ArgGroup::new("dialer").args(["server", "socks5_proxy"]))]
pub struct Args {
    /// The address to listen on, for both UDP and TCP
    #[arg(long, env = "LISTEN", default_value = "0.0.0.0:53")]
    listen: SocketAddr,

    /// Upstream DNS server: an IP, udp://, tcp://, tls:// or https:// URL. Repeatable or
    /// comma-separated.
    #[arg(long, env = "UPSTREAM", required = true, value_delimiter = ',')]
    upstream: Vec<ServerSpec>,

    /// Alternative DNS server, in the same format as --upstream. Repeatable or comma-separated.
    #[arg(long, env = "ALTERNATIVE", required = true, value_delimiter = ',')]
    alternative: Vec<ServerSpec>,

    /// The cpxy server URL with its key, `http://:<key>@<host>:<port>`, that the servers given
    /// with `?via=cpxy` are reached through
    #[arg(long, env = "SERVER", hide_env_values = true)]
    server: Option<ServerConfig>,

    /// A plaintext, no-auth SOCKS5 proxy (e.g. `cpxy client`'s) that the servers given with
    /// `?via=cpxy` are reached through, instead of --server
    #[arg(long, env = "SOCKS5_PROXY")]
    socks5_proxy: Option<SocketAddr>,

    /// How the DNS servers are reached; `cn_split` needs --server or --socks5-proxy
    #[arg(
        long,
        env = "RESOLVER_ROUTING",
        value_enum,
        default_value_t = ResolverRouting::Explicit,
        requires_if("cn_split", "dialer")
    )]
    resolver_routing: ResolverRouting,

    /// With `--resolver-routing cn_split`: CIDRs whose servers go through the proxy even when
    /// they are in China. Repeatable or comma-separated.
    #[arg(long, env = "PROXY_ROUTES", value_delimiter = ',', value_parser = ProxyRoutes::parse_network)]
    proxy_route: Vec<ipnet::IpNet>,

    /// How long to wait for answers to a query, e.g. `5s` or `1500ms`; a bare number is seconds
    #[arg(long, env = "TIMEOUT", default_value = "5s", value_parser = parse_duration)]
    timeout: Duration,

    /// Where to cache answers: `memory`, `off`, or the path of a SQLite file, which keeps them
    /// across restarts
    #[arg(long, env = "CACHE", default_value = "memory")]
    cache: Cache,

    /// Minimum time to cache an answer, in the same format as --timeout
    #[arg(long, env = "CACHE_MIN_TTL", default_value = "30s", value_parser = parse_duration)]
    cache_min_ttl: Duration,

    /// Maximum time to cache an answer, in the same format as --timeout
    #[arg(long, env = "CACHE_MAX_TTL", default_value = "1d", value_parser = parse_duration)]
    cache_max_ttl: Duration,

    /// Maximum time to cache a negative answer (no such name, or no records), in the same format
    /// as --timeout
    #[arg(long, env = "CACHE_NEGATIVE_TTL", default_value = "5m", value_parser = parse_duration)]
    cache_negative_ttl: Duration,

    /// The local network's own resolver (e.g. dnsmasq), in the same format as --upstream. It
    /// answers single-label names, names under --local-domain and reverse lookups of private
    /// addresses, which are not split or cached.
    #[arg(long, env = "LOCAL_SERVER")]
    local_server: Option<ServerSpec>,

    /// A domain only the local resolver knows, e.g. `lan`. Repeatable or comma-separated; needs
    /// --local-server.
    #[arg(
        long,
        env = "LOCAL_DOMAINS",
        value_delimiter = ',',
        requires = "local_server"
    )]
    local_domain: Vec<Name>,

    /// Suffix to try for single-label names, in order, before the bare name. Repeatable or
    /// comma-separated.
    #[arg(long, env = "SEARCH_DOMAINS", value_delimiter = ',', value_parser = super::search::parse_search_domain)]
    search_domain: Vec<Name>,

    /// IPv6 answer policy: off returns empty AAAA answers; direct retains local-region AAAA only.
    #[arg(long, env = "IPV6", value_enum, default_value = "off")]
    ipv6: Ipv6Mode,
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    let timeout = args.timeout;
    let tls_config = tls_client_config()?;
    let cpxy: Option<Arc<dyn Dialer>> = match (&args.server, args.socks5_proxy) {
        (Some(server), _) => {
            tracing::info!("Using cpxy server {server:?} for the servers via cpxy");
            Some(Arc::new(OutboundDialer(Arc::new(ProtocolOutbound::new(
                server.clone(),
            )))))
        }
        (None, Some(proxy)) => {
            tracing::info!("Using SOCKS5 proxy {proxy} for the servers via cpxy");
            Some(Arc::new(OutboundDialer(Arc::new(Socks5Outbound(proxy)))))
        }
        (None, None) => None,
    };
    let routes = match args.resolver_routing {
        ResolverRouting::Explicit => None,
        ResolverRouting::CnSplit => Some(ProxyRoutes(args.proxy_route.clone())),
    };
    let new_upstream = async |spec: &ServerSpec| {
        let spec = match &routes {
            Some(routes) => route_spec(spec, routes).await?,
            None => spec.clone(),
        };
        HickoryUpstream::new(&spec, tls_config.clone(), timeout, cpxy.clone()).await
    };

    let mut servers: Vec<(Group, Arc<dyn DnsUpstream>)> = Vec::new();
    for (group, specs) in [
        (Group::Upstream, &args.upstream),
        (Group::Alternative, &args.alternative),
    ] {
        for spec in specs {
            let server = new_upstream(spec)
                .await
                .with_context(|| format!("Error setting up {group} server {spec}"))?;
            tracing::info!("Using {group} server {}", server.name());
            servers.push((group, Arc::new(server)));
        }
    }

    anyhow::ensure!(
        args.cache_min_ttl <= args.cache_max_ttl,
        "--cache-min-ttl must not exceed --cache-max-ttl"
    );
    // DNS TTLs are whole seconds that fit in 32 bits.
    let ttl = |d: Duration| u32::try_from(d.as_secs()).unwrap_or(u32::MAX);
    let options = CacheOptions {
        min_ttl: ttl(args.cache_min_ttl),
        max_ttl: ttl(args.cache_max_ttl),
        negative_ttl: ttl(args.cache_negative_ttl),
    };
    let cache = match &args.cache {
        Cache::Off => None,
        Cache::Memory => Some(DnsCache::open_in_memory(options).await?),
        Cache::File(path) => Some(DnsCache::open(path, options).await?),
    };
    let cache = cache.map(|cache| {
        tracing::info!(cache = ?args.cache, "Caching answers");
        let cache = Arc::new(cache);
        tokio::spawn(purge_expired_every(cache.clone(), PURGE_INTERVAL));
        cache
    });

    let mut handler = DnsSplitHandler::new(Racer::new(servers, timeout, is_local_region_ip), cache)
        .with_search_domains(args.search_domain)
        .with_ipv6(args.ipv6);
    if let Some(spec) = &args.local_server {
        let server = if routes.is_some() {
            // The router's own resolver, on its own network.
            let mut local = spec.clone();
            local.via = Via::Direct;
            HickoryUpstream::new(&local, tls_config.clone(), timeout, None).await
        } else {
            new_upstream(spec).await
        }
        .with_context(|| format!("Error setting up local server {spec}"))?;
        tracing::info!("Using local server {} for local names", server.name());
        let domains = args.local_domain.iter().map(|domain| {
            let mut domain = domain.clone();
            domain.set_fqdn(true);
            domain
        });
        handler = handler.with_local(Arc::new(server), domains.collect());
    }
    let handler = Arc::new(handler);

    let udp = UdpSocket::bind(args.listen)
        .await
        .with_context(|| format!("Error binding UDP {}", args.listen))?;
    let tcp = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("Error binding TCP {}", args.listen))?;
    tracing::info!("Listening on {}", args.listen);

    tokio::select! {
        r = serve_udp(udp, handler.clone()) => r,
        r = serve_tcp(tcp, handler) => r,
    }
}

/// `<n>` seconds, or `<n>` followed by `ms`, `s`, `m`, `h` or `d`.
fn parse_duration(s: &str) -> Result<Duration, String> {
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (number, unit) = s.split_at(split);
    let n: u64 = number
        .parse()
        .map_err(|_| format!("expected a number with an optional unit, got `{s}`"))?;
    let scale = match unit {
        "ms" => return Ok(Duration::from_millis(n)),
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return Err(format!("unknown unit `{unit}`: use ms, s, m, h or d")),
    };
    n.checked_mul(scale)
        .map(Duration::from_secs)
        .ok_or_else(|| format!("`{s}` is too long"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("5").unwrap(), Duration::from_secs(5));
        assert_eq!(parse_duration("5s").unwrap(), Duration::from_secs(5));
        assert_eq!(
            parse_duration("1500ms").unwrap(),
            Duration::from_millis(1500)
        );
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
        for bad in ["", "s", "-1", "1.5s", "1w", "5 s"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cache_values() {
        assert_eq!("memory".parse::<Cache>().unwrap(), Cache::Memory);
        assert_eq!("off".parse::<Cache>().unwrap(), Cache::Off);
        assert_eq!(
            "/data/dns.sqlite".parse::<Cache>().unwrap(),
            Cache::File("/data/dns.sqlite".into())
        );
    }

    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        use clap::Parser;
        Cli::try_parse_from(std::iter::once("dns").chain(args.iter().copied())).map(|c| c.args)
    }

    #[test]
    fn cn_split_needs_one_way_through() {
        let base = ["--upstream", "223.5.5.5", "--alternative", "8.8.8.8"];
        let with = |extra: &[&str]| parse(&[&base[..], extra].concat());
        assert!(with(&["--resolver-routing", "cn_split"]).is_err());
        assert!(
            with(&[
                "--resolver-routing",
                "cn_split",
                "--socks5-proxy",
                "127.0.0.1:1080"
            ])
            .is_ok()
        );
        assert!(
            with(&[
                "--resolver-routing",
                "cn_split",
                "--server",
                "http://:k@h:1"
            ])
            .is_ok()
        );
        assert!(
            with(&[
                "--server",
                "http://:k@h:1",
                "--socks5-proxy",
                "127.0.0.1:1080"
            ])
            .is_err()
        );
        assert!(with(&[]).is_ok());
    }

    /// A router's DNS: both listeners answer, and with IPv6 off, AAAA is answered empty without
    /// asking any server.
    #[tokio::test]
    async fn router_answers_on_udp_and_tcp() {
        use hickory_proto::op::{Message, Query, ResponseCode};
        use hickory_proto::rr::RecordType;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listen = UdpSocket::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        // A resolver that must not be asked, on both transports.
        let resolver_udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let resolver = resolver_udp.local_addr().unwrap();
        let resolver_tcp = TcpListener::bind(resolver).await.unwrap();
        let spec = format!("udp://{resolver}");
        let args = parse(&[
            "--listen",
            &listen.to_string(),
            "--upstream",
            &spec,
            "--alternative",
            &spec,
            "--local-server",
            &spec,
            "--local-domain",
            "lan",
            "--resolver-routing",
            "cn_split",
            "--socks5-proxy",
            "127.0.0.1:1",
        ])
        .unwrap();
        let server = tokio::spawn(run(args));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::net::TcpStream::connect(listen).await.is_err() {
            assert!(tokio::time::Instant::now() < deadline, "DNS did not start");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let mut query = Message::new();
        query.set_id(7).add_query(Query::query(
            Name::from_ascii("example.com.").unwrap(),
            RecordType::AAAA,
        ));
        let query = query.to_vec().unwrap();
        let check = |bytes: &[u8]| {
            let response = Message::from_vec(bytes).unwrap();
            assert_eq!(response.id(), 7);
            assert_eq!(response.response_code(), ResponseCode::NoError);
            assert!(response.answers().is_empty());
        };

        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        udp.send_to(&query, listen).await.unwrap();
        let mut buf = [0; 512];
        let n = tokio::time::timeout(Duration::from_secs(5), udp.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        check(&buf[..n]);

        let mut tcp = tokio::net::TcpStream::connect(listen).await.unwrap();
        tcp.write_u16(query.len() as u16).await.unwrap();
        tcp.write_all(&query).await.unwrap();
        let len = tcp.read_u16().await.unwrap();
        let mut reply = vec![0; len.into()];
        tcp.read_exact(&mut reply).await.unwrap();
        check(&reply);

        assert!(resolver_udp.try_recv(&mut buf).is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), resolver_tcp.accept())
                .await
                .is_err()
        );
        server.abort();
    }
}
