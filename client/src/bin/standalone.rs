use anyhow::Context;
use clap::Parser;
use client::client::UpstreamConfiguration;
use cpxy_ng::key_util::derive_password;
use dotenvy::dotenv;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio::try_join;

#[derive(clap::Parser)]
struct CliOptions {
    /// The cpxy sever host to connect to
    #[clap(long, env)]
    server_host: String,

    /// The cpxy server port to connect to
    #[clap(long, env)]
    server_port: u16,

    /// Whether to use TLS when connecting to the cpxy server
    #[clap(long, env, default_value_t = false)]
    server_tls: bool,

    /// The pre-shared key for encryption/decryption
    #[clap(long, env)]
    key: String,

    /// The address to listen on for the http proxy
    #[clap(long, env)]
    http_proxy_listen: Option<SocketAddr>,

    /// The address to listen on for the socks5 proxy
    #[clap(long, env)]
    socks5_proxy_listen: Option<SocketAddr>,
}

#[tokio::main]
async fn main() {
    let _ = dotenv();
    tracing_subscriber::fmt::init();

    let CliOptions {
        server_host,
        server_port,
        server_tls,
        key,
        http_proxy_listen,
        socks5_proxy_listen,
    } = CliOptions::parse();

    let key = derive_password(&key).into();

    let config = Arc::new(UpstreamConfiguration {
        host: server_host,
        port: server_port,
        tls: server_tls,
        key,
    });

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

            tokio::spawn(client::client::accept_http_proxy_connection(
                client,
                config.clone(),
            ));
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

            tokio::spawn(client::client::accept_socks_proxy_connection(
                BufReader::new(client),
                config.clone(),
            ));
        }
    };

    try_join!(run_http_proxy, run_socks5_proxy).unwrap();
}
