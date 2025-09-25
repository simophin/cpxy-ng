use anyhow::Context;
use clap::Parser;
use client::dns_divert::{Action, divert_action};
use client::handshaker::Handshaker;

use client::http_proxy_server::HttpProxyHandshaker;
use client::protocol_handlers::send_protocol_request;
use client::{http_proxy_server, protocol_config};
use cpxy_ng::http_proxy;
use cpxy_ng::tls_stream::TlsClientStream;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, copy_bidirectional};
use tokio::net::TcpStream;
use tracing::{Instrument, info_span, instrument};
use url::Url;

#[derive(clap::Parser)]
struct CliOptions {
    /// The cpxy sever host to connect to
    #[clap(env, required = true)]
    servers: Vec<Url>,

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

    let configs = Arc::new(
        servers
            .into_iter()
            .map(protocol_config::Config::try_from)
            .collect::<anyhow::Result<Vec<_>>>()
            .expect("Error parsing server URLs"),
    );

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
            tokio::spawn(serve_http_proxy_connection(conn, configs.clone()));
        }
    }
    .instrument(info_span!("http_proxy"));
}

#[instrument(skip(conn, configs), ret)]
async fn serve_http_proxy_connection(
    conn: TcpStream,
    configs: Arc<Vec<protocol_config::Config>>,
) -> anyhow::Result<()> {
    let (req, mut conn) = HttpProxyHandshaker::accept(conn).await?;

    let (domain, port) = match &req {
        http_proxy::ProxyRequest::Http(r) => (r.host.as_str(), r.port),
        http_proxy::ProxyRequest::Socket(r) => (r.host.as_str(), r.port),
    };

    match divert_action(domain).await {
        Ok(Action::Direct(addr)) => match req {
            http_proxy::ProxyRequest::Http(r) => {
                let upstream = async {
                    let upstream = TcpStream::connect((addr, port))
                        .await
                        .context("Error connecting to remote")?;

                    let mut upstream = if r.tls {
                        TlsClientStream::connect_tls(&r.host, upstream)
                            .await
                            .context("Error establishing TLS connection to remote")?
                    } else {
                        TlsClientStream::Plain(upstream)
                    };

                    upstream
                        .write_all(&r.payload)
                        .await
                        .context("Error sending initial payload to remote")?;

                    anyhow::Ok(upstream)
                }
                .await;

                match upstream {
                    Ok(mut upstream) => {
                        let mut conn = conn.respond_ok().await?;
                        let _ = copy_bidirectional(&mut upstream, &mut conn).await;
                        Ok(())
                    }

                    Err(e) => {
                        conn.respond_err(format!("{e:?}").as_str()).await?;
                        Err(e).context("Error connecting to upstream")
                    }
                }
            }

            http_proxy::ProxyRequest::Socket(_) => match TcpStream::connect((addr, port)).await {
                Ok(mut upstream) => {
                    let mut conn = conn.respond_ok().await?;
                    let _ = copy_bidirectional(&mut upstream, &mut conn).await;
                    Ok(())
                }

                Err(e) => {
                    conn.respond_err(format!("{e:?}").as_str()).await?;
                    Err(e).context("Error connecting to upstream")
                }
            },
        },

        Ok(Action::Proxy) => {
            let config = configs.as_slice().get(0).unwrap();
            let upstream = async {
                let req = http_proxy_server::http_proxy_request_to_protocol_request(
                    req,
                    conn.stream_mut(),
                    config,
                )
                .await?;

                send_protocol_request(config, req).await
            };

            match upstream.await {
                Ok(mut upstream) => {
                    let mut conn = conn.respond_ok().await?;
                    let _ = copy_bidirectional(&mut upstream, &mut conn).await;
                    Ok(())
                }

                Err(e) => {
                    conn.respond_err(format!("{e:?}").as_str()).await?;
                    Err(e).context("Error connecting to upstream")
                }
            }
        }

        Err(e) => {
            conn.respond_err(format!("{e:?}").as_str()).await?;
            Err(e).context("Error determining divert action")
        }
    }
}
