//! `cpxy client`: local HTTP, SOCKS5 and transparent proxies that send traffic through a cpxy
//! server.

use crate::http_proxy_server::HttpProxyHandshaker;
use crate::outbound::{ProtocolOutbound, ProxyRoutes, StatReportingOutbound, cn};
use crate::protocol_config::Config;
use crate::proxy_handlers::serve_listener;
use crate::socks_proxy_server::SocksProxyHandshaker;
use crate::stats_server::{OutboundEvent, StatsProvider, serve_stats};
use anyhow::Context;
use cpxy_ng::outbound::Outbound;
use std::borrow::Cow;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tracing::{Instrument, info_span};

/// Which destinations go through the cpxy server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Routing {
    /// Every destination
    Global,
    /// Destinations outside China, by GeoIP; Chinese, private and tailnet addresses connect
    /// directly. Domains are resolved with --dns-server to decide.
    #[value(name = "cn_split")]
    CnSplit,
}

#[derive(Debug, clap::Args)]
#[command(group = clap::ArgGroup::new("listeners").multiple(true).required(true))]
pub struct Args {
    /// The cpxy server URL with its key, `https://:<key>@<host>:<port>`
    #[arg(long, env = "SERVER", hide_env_values = true)]
    server: Config,

    /// Which destinations go through the server
    #[arg(long, env = "ROUTING", value_enum, default_value_t = Routing::Global)]
    routing: Routing,

    /// With `--routing cn_split`: CIDRs that always go through the server, even when they are
    /// in China. Repeatable or comma-separated.
    #[arg(long, env = "PROXY_ROUTES", value_delimiter = ',', value_parser = ProxyRoutes::parse_network)]
    proxy_route: Vec<ipnet::IpNet>,

    /// With `--routing cn_split`: the DNS servers that resolve domains to decide their route.
    /// Repeatable or comma-separated.
    #[arg(
        long,
        env = "DNS_SERVER",
        value_delimiter = ',',
        default_value = "8.8.8.8"
    )]
    dns_server: Vec<IpAddr>,

    /// The address to listen on for the HTTP proxy
    #[arg(long, env = "HTTP_PROXY_LISTEN", group = "listeners")]
    http_proxy_listen: Option<SocketAddr>,

    /// The address to listen on for the SOCKS5 proxy
    #[arg(long, env = "SOCKS5_PROXY_LISTEN", group = "listeners")]
    socks5_proxy_listen: Option<SocketAddr>,

    /// The address to listen on for connections redirected by the kernel (an nftables
    /// `redirect`), proxied to their original destination. Linux only.
    #[arg(long, env = "TRANSPARENT_PROXY_LISTEN", group = "listeners")]
    transparent_proxy_listen: Option<SocketAddr>,

    /// The address to serve the connection events API on (`GET /events`)
    #[arg(long, env = "API_LISTEN")]
    api_listen: Option<SocketAddr>,
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    #[cfg(not(target_os = "linux"))]
    if args.transparent_proxy_listen.is_some() {
        anyhow::bail!("--transparent-proxy-listen is only supported on Linux");
    }

    let (events_tx, events_rx) = broadcast::channel(1024);
    tracing::info!(server = ?args.server, routing = ?args.routing, "Client: starting");

    match args.routing {
        Routing::Global => {
            let outbound = StatReportingOutbound {
                name: Cow::Borrowed("global"),
                inner: ProtocolOutbound(args.server.clone()),
                events_tx,
            };
            serve(&args, Arc::new(outbound), events_rx).await
        }
        Routing::CnSplit => {
            let outbound = cn::cn_outbound(
                args.dns_server
                    .iter()
                    .map(|ip| SocketAddr::new(*ip, 53))
                    .collect(),
                args.server.clone(),
                ProxyRoutes(args.proxy_route.clone()),
                events_tx,
            );
            serve(&args, Arc::new(outbound), events_rx).await
        }
    }
}

async fn serve<O>(
    args: &Args,
    outbound: Arc<O>,
    events: broadcast::Receiver<OutboundEvent>,
) -> anyhow::Result<()>
where
    O: Outbound + Send + Sync + 'static,
{
    let http = async {
        let Some(listen) = args.http_proxy_listen else {
            return std::future::pending().await;
        };
        let listener = TcpListener::bind(listen)
            .await
            .with_context(|| format!("Error binding HTTP proxy on {listen}"))?;
        tracing::info!("HTTP proxy listening on {}", listener.local_addr()?);
        serve_listener::<HttpProxyHandshaker<_>, _>(listener, outbound.clone()).await
    }
    .instrument(info_span!("http_proxy"));

    let socks5 = async {
        let Some(listen) = args.socks5_proxy_listen else {
            return std::future::pending().await;
        };
        let listener = TcpListener::bind(listen)
            .await
            .with_context(|| format!("Error binding SOCKS5 proxy on {listen}"))?;
        tracing::info!("SOCKS5 proxy listening on {}", listener.local_addr()?);
        serve_listener::<SocksProxyHandshaker<_>, _>(listener, outbound.clone()).await
    }
    .instrument(info_span!("socks5_proxy"));

    let transparent = async {
        let Some(listen) = args.transparent_proxy_listen else {
            return std::future::pending().await;
        };
        #[cfg(target_os = "linux")]
        {
            let listener = TcpListener::bind(listen)
                .await
                .with_context(|| format!("Error binding transparent proxy on {listen}"))?;
            tracing::info!("Transparent proxy listening on {}", listener.local_addr()?);
            serve_listener::<crate::transparent_proxy_server::TransparentHandshaker, _>(
                listener,
                outbound.clone(),
            )
            .await
        }
        #[cfg(not(target_os = "linux"))]
        unreachable!("{listen}")
    }
    .instrument(info_span!("transparent_proxy"));

    let api = async {
        let Some(listen) = args.api_listen else {
            return std::future::pending().await;
        };
        let listener = TcpListener::bind(listen)
            .await
            .with_context(|| format!("Error binding API on {listen}"))?;
        tracing::info!("API listening on {}", listener.local_addr()?);
        serve_stats(StatsProvider { events }, listener).await
    }
    .instrument(info_span!("api"));

    // Every listener serves until it fails, so the first result is an error.
    tokio::select! {
        r = http => r,
        r = socks5 => r,
        r = transparent => r,
        r = api => r,
    }
}
