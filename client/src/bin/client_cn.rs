use anyhow::Context;
use clap::Parser;

use client::http_proxy_server::HttpProxyHandshaker;
use client::outbound::cn;
use client::protocol_config::Config;
use client::proxy_handlers::serve_listener;
use client::socks_proxy_server::SocksProxyHandshaker;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::try_join;
use tracing::{Instrument, info_span};

#[derive(clap::Parser)]
struct CliOptions {
    /// The cpxy sever host to connect to
    #[clap(env, required = true)]
    server: Config,

    /// The cpxy server host to connect to for AI website access
    #[clap(env, long)]
    ai_server: Option<Config>,

    /// The cpxy server host to connect to for Tailscale access
    #[clap(env, long)]
    tailscale_server: Option<Config>,

    /// The address to listen on for the http proxy
    #[clap(long, env)]
    http_proxy_listen: Option<SocketAddr>,

    /// The address to listen on for the socks5 proxy
    #[clap(long, env)]
    socks5_proxy_listen: Option<SocketAddr>,
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();

    let CliOptions {
        server,
        http_proxy_listen,
        socks5_proxy_listen,
        ai_server,
        tailscale_server,
    } = CliOptions::parse();

    tracing::info!(
        "Using servers: {server:?}, AI divert: {ai_server:?}, Tailscale divert: {tailscale_server:?}"
    );

    let outbound = Arc::new(cn::cn_outbound(server, ai_server, tailscale_server));

    let run_http_proxy = async {
        let Some(listen) = http_proxy_listen else {
            return anyhow::Ok(());
        };

        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .context("Error binding HTTP proxy listen address")?;

        tracing::info!("HTTP proxy listening on {}", listener.local_addr()?);
        serve_listener::<HttpProxyHandshaker<_>, _>(listener, outbound.clone()).await
    }
    .instrument(info_span!("http_proxy"));

    let run_socks_proxy = async {
        let Some(listen) = socks5_proxy_listen else {
            return anyhow::Ok(());
        };

        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .context("Error binding SOCKS5 proxy listen address")?;

        tracing::info!("SOCKS5 proxy listening on {}", listener.local_addr()?);
        serve_listener::<SocksProxyHandshaker<_>, _>(listener, outbound.clone()).await
    }
    .instrument(info_span!("socks5_proxy"));

    try_join!(run_http_proxy, run_socks_proxy).expect("To run proxy server");
}
