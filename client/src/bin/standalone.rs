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
    /// The address to listen on for the http proxy
    #[clap(long, env)]
    http_proxy_listen: Option<SocketAddr>,

    /// The address to listen on for the socks5 proxy
    #[clap(long, env)]
    socks5_proxy_listen: Option<SocketAddr>,

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
    } = CliOptions::parse();

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

    try_join!(run_http_proxy, run_socks5_proxy).unwrap();
}
