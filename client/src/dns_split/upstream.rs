use super::spec::ServerSpec;
use anyhow::Context;
use futures::future::BoxFuture;
use hickory_proto::h2::HttpsClientStreamBuilder;
use hickory_proto::op::Message;
use hickory_proto::runtime::{TokioRuntimeProvider, TokioTime};
use hickory_proto::rustls::tls_client_connect;
use hickory_proto::tcp::TcpClientStream;
use hickory_proto::udp::UdpClientStream;
use hickory_proto::xfer::{
    DnsExchange, DnsHandle, DnsMultiplexer, DnsRequest, DnsRequestOptions, FirstAnswer,
};
use rustls::{ClientConfig, RootCertStore};
use std::fmt::{Display, Formatter};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Which group a DNS server was configured in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Upstream,
    Alternative,
}

impl Group {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Upstream => "upstream",
            Self::Alternative => "alternative",
        }
    }
}

impl Display for Group {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A DNS server that answers a query with a full response message.
pub trait DnsUpstream: Send + Sync {
    fn name(&self) -> &str;
    fn query(&self, request: Message) -> BoxFuture<'_, anyhow::Result<Message>>;
}

pub fn tls_client_config() -> anyhow::Result<Arc<ClientConfig>> {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };

    Ok(Arc::new(
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .context("Error configuring TLS protocol versions")?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

enum Transport {
    Udp,
    Tcp,
    Tls { server_name: String },
    Https { server_name: String, path: String },
}

/// A DNS server reached over UDP, TCP, TLS or HTTPS. The connection is created on first use,
/// kept for later queries, and re-created after it fails.
pub struct HickoryUpstream {
    name: String,
    addr: SocketAddr,
    transport: Transport,
    tls_config: Arc<ClientConfig>,
    timeout: Duration,
    exchange: Mutex<Option<DnsExchange>>,
}

impl HickoryUpstream {
    pub async fn new(
        spec: &ServerSpec,
        tls_config: Arc<ClientConfig>,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        let addr = spec.resolve_addr().await?;
        let transport = match spec {
            ServerSpec::Udp(_) => Transport::Udp,
            ServerSpec::Tcp(_) => Transport::Tcp,
            ServerSpec::Tls { host, .. } => Transport::Tls {
                server_name: host.clone(),
            },
            ServerSpec::Https { host, path, .. } => Transport::Https {
                server_name: host.clone(),
                path: path.clone(),
            },
        };

        Ok(Self {
            name: format!("{spec} ({addr})"),
            addr,
            transport,
            tls_config,
            timeout,
            exchange: Mutex::new(None),
        })
    }

    async fn connect(&self) -> anyhow::Result<DnsExchange> {
        let provider = TokioRuntimeProvider::default();
        let (exchange, background) = match &self.transport {
            Transport::Udp => {
                let stream = UdpClientStream::builder(self.addr, provider)
                    .with_timeout(Some(self.timeout))
                    .build();
                let (exchange, background) =
                    DnsExchange::connect::<_, _, TokioTime>(stream).await?;
                (exchange, tokio::spawn(background))
            }
            Transport::Tcp => {
                let (stream, handle) =
                    TcpClientStream::new(self.addr, None, Some(self.timeout), provider);
                let multiplexer = DnsMultiplexer::with_timeout(stream, handle, self.timeout, None);
                let (exchange, background) =
                    DnsExchange::connect::<_, _, TokioTime>(multiplexer).await?;
                (exchange, tokio::spawn(background))
            }
            Transport::Tls { server_name } => {
                let (stream, handle) = tls_client_connect(
                    self.addr,
                    server_name.clone(),
                    self.tls_config.clone(),
                    provider,
                );
                let multiplexer = DnsMultiplexer::with_timeout(stream, handle, self.timeout, None);
                let (exchange, background) =
                    DnsExchange::connect::<_, _, TokioTime>(multiplexer).await?;
                (exchange, tokio::spawn(background))
            }
            Transport::Https { server_name, path } => {
                let stream =
                    HttpsClientStreamBuilder::with_client_config(self.tls_config.clone(), provider)
                        .build(self.addr, server_name.clone(), path.clone());
                let (exchange, background) =
                    DnsExchange::connect::<_, _, TokioTime>(stream).await?;
                (exchange, tokio::spawn(background))
            }
        };

        // The background task ends by itself when the connection closes.
        drop(background);
        Ok(exchange)
    }

    /// Returns the cached connection, or a new one. The flag tells whether it is new.
    async fn exchange(&self) -> anyhow::Result<(DnsExchange, bool)> {
        let mut guard = self.exchange.lock().await;
        if let Some(exchange) = guard.as_ref() {
            return Ok((exchange.clone(), false));
        }

        let exchange = self
            .connect()
            .await
            .with_context(|| format!("Error connecting to {}", self.name))?;
        *guard = Some(exchange.clone());
        Ok((exchange, true))
    }

    async fn send(&self, exchange: DnsExchange, request: Message) -> anyhow::Result<Message> {
        let response = exchange
            .send(DnsRequest::new(request, DnsRequestOptions::default()))
            .first_answer()
            .await;

        match response {
            Ok(response) => Ok(response.into_message()),
            Err(e) => {
                self.exchange.lock().await.take();
                Err(e).with_context(|| format!("Error querying {}", self.name))
            }
        }
    }

    async fn query_inner(&self, request: Message) -> anyhow::Result<Message> {
        let (exchange, is_new) = self.exchange().await?;
        match self.send(exchange, request.clone()).await {
            Ok(response) => Ok(response),
            // A kept connection may have been closed by the server while idle, retry once on a new one.
            Err(e) if !is_new => {
                tracing::debug!("Retrying on a new connection: {e:?}");
                let (exchange, _) = self.exchange().await?;
                self.send(exchange, request).await
            }
            Err(e) => Err(e),
        }
    }
}

impl DnsUpstream for HickoryUpstream {
    fn name(&self) -> &str {
        &self.name
    }

    fn query(&self, request: Message) -> BoxFuture<'_, anyhow::Result<Message>> {
        Box::pin(self.query_inner(request))
    }
}
