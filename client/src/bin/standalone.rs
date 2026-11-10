use anyhow::Context;
use clap::Parser;
use client::http_proxy_server::HttpProxyHandshaker;
use client::outbound::ProtocolOutbound;
use client::protocol_config::Config;
use client::proxy_handlers::serve_listener;
use client::socks_proxy_server::SocksProxyHandshaker;
use dotenvy::dotenv;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::try_join;

#[derive(clap::Parser)]
struct CliOptions {
    /// CIDRs forced through the main proxy (repeatable or comma-separated).
    #[arg(long, env = "PROXY_ROUTES", value_delimiter = ',', value_parser = client::outbound::ProxyRoutes::parse_network)]
    proxy_route: Vec<ipnet::IpNet>,

    /// The address to listen on for the http proxy
    #[clap(long, env)]
    http_proxy_listen: Option<SocketAddr>,

    /// The address to listen on for the socks5 proxy
    #[clap(long, env)]
    socks5_proxy_listen: Option<SocketAddr>,

    /// The address to listen on for connections redirected by the kernel (an nftables
    /// `redirect`), proxied to their original destination. Linux only.
    #[clap(long, env)]
    transparent_proxy_listen: Option<SocketAddr>,

    /// The server configuration
    #[clap(env)]
    config: Config,
}

#[tokio::main]
async fn main() {
    let _ = dotenv();
    tracing_subscriber::fmt::init();

    let CliOptions {
        config,
        http_proxy_listen,
        socks5_proxy_listen,
        transparent_proxy_listen,
        proxy_route,
    } = CliOptions::parse();

    #[cfg(not(target_os = "linux"))]
    if transparent_proxy_listen.is_some() {
        eprintln!("--transparent-proxy-listen is only supported on Linux");
        std::process::exit(2);
    }

    // This binary already sends every destination through the proxy. Parse the same route
    // option for configuration compatibility with the split-routing binaries.
    let _proxy_routes = client::outbound::ProxyRoutes(proxy_route);
    let outbound = Arc::new(ProtocolOutbound(config));

    let run_http_proxy = async {
        let Some(listen) = http_proxy_listen else {
            return anyhow::Ok(());
        };

        let listener = TcpListener::bind(listen)
            .await
            .context("Error binding HTTP proxy listen address")?;

        tracing::info!("HTTP proxy listening on {}", listener.local_addr()?);

        serve_listener::<HttpProxyHandshaker<_>, _>(listener, outbound.clone()).await
    };

    let run_socks5_proxy = async {
        let Some(listen) = socks5_proxy_listen else {
            return Ok(());
        };

        let listener = TcpListener::bind(listen)
            .await
            .context("Error binding SOCKS5 proxy listen address")?;

        tracing::info!("SOCKS5 proxy listening on {}", listener.local_addr()?);
        serve_listener::<SocksProxyHandshaker<_>, _>(listener, outbound.clone()).await
    };

    let run_transparent_proxy = async {
        let Some(listen) = transparent_proxy_listen else {
            return Ok(());
        };

        #[cfg(target_os = "linux")]
        {
            let listener = TcpListener::bind(listen)
                .await
                .context("Error binding transparent proxy listen address")?;

            tracing::info!("Transparent proxy listening on {}", listener.local_addr()?);
            serve_listener::<client::transparent_proxy_server::TransparentHandshaker, _>(
                listener,
                outbound.clone(),
            )
            .await
        }
        #[cfg(not(target_os = "linux"))]
        unreachable!("{listen}")
    };

    try_join!(run_http_proxy, run_socks5_proxy, run_transparent_proxy).unwrap();
}
