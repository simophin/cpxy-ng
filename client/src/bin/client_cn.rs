use anyhow::Context;
use clap::Parser;
use client::dns_divert::{Action, divert_action};
use std::fmt::Debug;

use client::direct_outbound::DirectOutbound;
use client::either_stream::EitherStream;
use client::http_proxy_server::HttpProxyHandshaker;
use client::protocol_config::Config;
use client::protocol_outbound::ProtocolOutbound;
use client::proxy_handlers;
use client::selector_outbound::SelectorOutbound;
use client::socks_proxy_server::SocksProxyHandshaker;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::net::TcpStream;
use tokio::try_join;
use tracing::{Instrument, info_span, instrument};

#[derive(clap::Parser)]
struct CliOptions {
    /// The cpxy sever host to connect to
    #[clap(env, required = true, value_delimiter = ',')]
    servers: Vec<Config>,

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
        servers,
        http_proxy_listen,
        socks5_proxy_listen,
    } = CliOptions::parse();

    tracing::info!("Using servers: {servers:?}");

    let global_outbound =
        Arc::new(SelectorOutbound::new(servers.into_iter().map(ProtocolOutbound)).unwrap());

    let run_http_proxy = async {
        let Some(listen) = http_proxy_listen else {
            return anyhow::Ok(());
        };

        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .context("Error binding HTTP proxy listen address")?;

        tracing::info!("HTTP proxy listening on {}", listener.local_addr()?);

        loop {
            let (conn, peer_addr) = listener
                .accept()
                .await
                .context("Error accepting HTTP proxy connection")?;
            tracing::info!("Accepted HTTP proxy connection from {peer_addr}");
            tokio::spawn(serve_http_proxy_connection(conn, global_outbound.clone()));
        }
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

        loop {
            let (conn, peer_addr) = listener
                .accept()
                .await
                .context("Error accepting SOCKS5 proxy connection")?;
            tracing::info!("Accepted SOCKS5 proxy connection from {peer_addr}");
            tokio::spawn(serve_socks5_proxy_connection(conn, global_outbound.clone()));
        }
    }
    .instrument(info_span!("socks5_proxy"));

    try_join!(run_http_proxy, run_socks_proxy).expect("To run proxy server");
}

#[instrument(skip(conn, global_outbound), ret)]
async fn serve_http_proxy_connection(
    conn: TcpStream,
    global_outbound: impl Outbound + Debug,
) -> anyhow::Result<()> {
    proxy_handlers::serve::<HttpProxyHandshaker<_>, _, _>(
        conn,
        CnDivertOutbound {
            cn_outbound: DirectOutbound,
            global_outbound,
        },
    )
    .await
}

#[instrument(skip(conn), ret)]
async fn serve_socks5_proxy_connection(
    conn: TcpStream,
    global_outbound: impl Outbound + Debug,
) -> anyhow::Result<()> {
    proxy_handlers::serve::<SocksProxyHandshaker<_>, _, _>(
        BufReader::new(conn),
        CnDivertOutbound {
            cn_outbound: DirectOutbound,
            global_outbound,
        },
    )
    .await
}

pub struct CnDivertOutbound<CN, Global> {
    pub cn_outbound: CN,
    pub global_outbound: Global,
}

impl<CN, Global> Outbound for CnDivertOutbound<CN, Global>
where
    CN: Outbound,
    Global: Outbound,
{
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Unpin> {
        match divert_action(req.host.as_str())
            .await
            .context("Error resolving DNS")?
        {
            Action::Direct(addr) => self
                .cn_outbound
                .send(OutboundRequest {
                    host: addr.to_string(),
                    port: req.port,
                    tls: req.tls,
                    initial_plaintext: req.initial_plaintext,
                })
                .await
                .context("Error sending via CN outbound")
                .map(EitherStream::Left),

            Action::Proxy => self
                .global_outbound
                .send(req)
                .await
                .context("Error sending global outbound")
                .map(EitherStream::Right),
        }
    }
}
