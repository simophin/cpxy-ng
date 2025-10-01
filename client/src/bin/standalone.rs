use anyhow::Context;
use clap::Parser;
use client::http_proxy_server::HttpProxyHandshaker;
use client::outbound::ProtocolOutbound;
use client::protocol_config::Config;
use client::proxy_handlers;
use client::socks_proxy_server::SocksProxyHandshaker;
use cpxy_ng::outbound::Outbound;
use dotenvy::dotenv;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::BufReader;
use tokio::net::{TcpListener, TcpStream};
use tokio::try_join;
use tracing::instrument;

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

        loop {
            let (client, addr) = listener.accept().await.expect("Error accepting connection");
            tracing::info!("Accepted connection from {addr}");
            tokio::spawn(handle_http_proxy_conn(client, outbound.clone()));
        }
    };

    let run_socks5_proxy = async {
        let Some(listen) = socks5_proxy_listen else {
            return Ok(());
        };

        let listener = TcpListener::bind(listen)
            .await
            .context("Error binding SOCKS5 proxy listen address")?;

        tracing::info!("SOCKS5 proxy listening on {}", listener.local_addr()?);

        loop {
            let (client, addr) = listener.accept().await.expect("Error accepting connection");
            tracing::info!("Accepted connection from {addr}");
            tokio::spawn(handle_socks_proxy_conn(client, outbound.clone()));
        }
    };

    try_join!(run_http_proxy, run_socks5_proxy).unwrap();
}

#[instrument(ret, skip(conn))]
async fn handle_socks_proxy_conn(
    conn: TcpStream,
    outbound: impl Outbound + Debug,
) -> anyhow::Result<()> {
    proxy_handlers::serve::<SocksProxyHandshaker<_>, _, _>(BufReader::new(conn), outbound).await
}

#[instrument(ret, skip(conn))]
async fn handle_http_proxy_conn(
    conn: TcpStream,
    outbound: impl Outbound + Debug,
) -> anyhow::Result<()> {
    proxy_handlers::serve::<HttpProxyHandshaker<_>, _, _>(conn, outbound).await
}
