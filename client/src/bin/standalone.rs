use anyhow::Context;
use clap::Parser;
use client::handshaker::Handshaker;
use client::http_proxy_server::{HttpProxyHandshaker, http_proxy_request_to_protocol_request};
use client::protocol_config::Config;
use client::protocol_handlers::send_protocol_request;
use client::socks_proxy_server::{SocksProxyHandshaker, socks_proxy_request_to_protocol_request};
use dotenvy::dotenv;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{BufReader, copy_bidirectional};
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

    let config = Arc::new(config);

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
            tokio::spawn(handle_http_proxy_conn(client, config.clone()));
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
            tokio::spawn(handle_socks_proxy_conn(client, config.clone()));
        }
    };

    try_join!(run_http_proxy, run_socks5_proxy).unwrap();
}

#[instrument(ret, skip(conn))]
async fn handle_socks_proxy_conn(
    conn: TcpStream,
    config: impl AsRef<Config> + Debug,
) -> anyhow::Result<()> {
    let (req, mut handshaker) = HttpProxyHandshaker::accept(conn).await?;

    let upstream = async {
        let req =
            http_proxy_request_to_protocol_request(req, handshaker.stream_mut(), config.as_ref())
                .await?;

        send_protocol_request(config.as_ref(), req).await
    }
    .await;

    match upstream {
        Ok(mut upstream) => {
            let mut conn = handshaker.respond_ok().await?;
            let _ = copy_bidirectional(&mut conn, &mut upstream).await;
            Ok(())
        }

        Err(e) => {
            handshaker.respond_err("Internal Server Error").await?;
            Err(e)
        }
    }
}

#[instrument(ret, skip(conn))]
async fn handle_http_proxy_conn(
    conn: TcpStream,
    config: impl AsRef<Config> + Debug,
) -> anyhow::Result<()> {
    let conn = BufReader::new(conn);
    let (req, mut handshaker) = SocksProxyHandshaker::accept(conn).await?;

    let upstream = async {
        let req =
            socks_proxy_request_to_protocol_request(req, handshaker.stream_mut(), config.as_ref())
                .await?;

        send_protocol_request(config.as_ref(), req).await
    }
    .await;

    match upstream {
        Ok(mut upstream) => {
            let mut conn = handshaker.respond_ok().await?;
            let _ = copy_bidirectional(&mut conn, &mut upstream).await;
            Ok(())
        }

        Err(e) => {
            handshaker.respond_err("Internal Server Error").await?;
            Err(e)
        }
    }
}
