use super::spec::{Endpoint, ServerSpec, Via};
use anyhow::Context as _;
use cpxy_ng::outbound::{Outbound, OutboundHost, OutboundRequest};
use futures::TryFutureExt;
use futures::future::BoxFuture;
use hickory_proto::ProtoError;
use hickory_proto::op::Message;
use hickory_proto::runtime::iocompat::AsyncIoTokioAsStd;
use hickory_proto::runtime::{TokioRuntimeProvider, TokioTime};
use hickory_proto::tcp::{TcpClientStream, TcpStream as DnsTcpStream};
use hickory_proto::udp::UdpClientStream;
use hickory_proto::xfer::{
    DnsExchange, DnsHandle, DnsMultiplexer, DnsRequest, DnsRequestOptions, FirstAnswer,
};
use std::fmt::{Display, Formatter};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
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

/// A TCP connection to a DNS server.
pub type DnsStream = Pin<Box<dyn AsyncReadWrite + Send + Sync>>;

pub trait AsyncReadWrite: AsyncRead + AsyncWrite {}

impl<T: AsyncRead + AsyncWrite> AsyncReadWrite for T {}

/// Opens TCP connections to DNS servers.
pub trait Dialer: Send + Sync {
    fn dial(&self, addr: SocketAddr) -> BoxFuture<'static, io::Result<DnsStream>>;
}

/// Connects from this host.
pub struct DirectDialer;

impl Dialer for DirectDialer {
    fn dial(&self, addr: SocketAddr) -> BoxFuture<'static, io::Result<DnsStream>> {
        Box::pin(async move {
            let stream = TcpStream::connect(addr).await?;
            stream.set_nodelay(true)?;
            Ok(Box::pin(stream) as DnsStream)
        })
    }
}

/// Connects through an [`Outbound`], such as the cpxy server's.
pub struct OutboundDialer<O>(pub Arc<O>);

impl<O: Outbound + Send + Sync + 'static> Dialer for OutboundDialer<O> {
    fn dial(&self, addr: SocketAddr) -> BoxFuture<'static, io::Result<DnsStream>> {
        let outbound = self.0.clone();
        Box::pin(async move {
            let stream = outbound
                .send(OutboundRequest {
                    host: OutboundHost::Resolved {
                        domain: addr.ip().to_string(),
                        ip: Some(addr.ip()),
                    },
                    port: addr.port(),
                    initial_plaintext: Vec::new(),
                })
                .await
                .map_err(io::Error::other)?;
            Ok(Box::pin(SyncStream(std::sync::Mutex::new(stream))) as DnsStream)
        })
    }
}

/// Makes a stream `Sync`, which hickory requires, without locking: the stream is only reached
/// through `Pin<&mut Self>`.
struct SyncStream<S>(std::sync::Mutex<S>);

impl<S> SyncStream<S> {
    fn inner(self: Pin<&mut Self>) -> Pin<&mut S>
    where
        S: Unpin,
    {
        let mutex = &mut self.get_mut().0;
        Pin::new(mutex.get_mut().unwrap_or_else(|e| e.into_inner()))
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for SyncStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.inner().poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for SyncStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.inner().poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.inner().poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.inner().poll_shutdown(cx)
    }
}

/// A DNS server reached over UDP or TCP. The connection is created on first use,
/// kept for later queries, and re-created after it fails.
pub struct HickoryUpstream {
    name: String,
    endpoint: Endpoint,
    dialer: Arc<dyn Dialer>,
    timeout: Duration,
    exchange: Mutex<Option<DnsExchange>>,
}

impl HickoryUpstream {
    /// `cpxy` opens the connections of a server whose spec has `?via=cpxy`. Without it, such a
    /// server is refused.
    pub fn new(
        spec: &ServerSpec,
        timeout: Duration,
        cpxy: Option<Arc<dyn Dialer>>,
    ) -> anyhow::Result<Self> {
        let dialer = match spec.via {
            Via::Direct => Arc::new(DirectDialer),
            Via::Cpxy => cpxy.with_context(|| format!("{spec} needs a cpxy server"))?,
        };

        Ok(Self {
            name: spec.to_string(),
            endpoint: spec.endpoint,
            dialer,
            timeout,
            exchange: Mutex::new(None),
        })
    }

    async fn connect(&self) -> anyhow::Result<DnsExchange> {
        let (exchange, background) = match self.endpoint {
            Endpoint::Udp(addr) => {
                let stream = UdpClientStream::builder(addr, TokioRuntimeProvider::default())
                    .with_timeout(Some(self.timeout))
                    .build();
                let (exchange, background) =
                    DnsExchange::connect::<_, _, TokioTime>(stream).await?;
                (exchange, tokio::spawn(background))
            }
            Endpoint::Tcp(addr) => {
                let dial = self.dialer.dial(addr);
                let dial = Box::pin(async move { dial.await.map(AsyncIoTokioAsStd) });
                let (stream, handle) = DnsTcpStream::with_future(dial, addr, self.timeout);
                let stream = Box::pin(
                    stream
                        .map_ok(TcpClientStream::from_stream)
                        .map_err(ProtoError::from),
                );
                let multiplexer = DnsMultiplexer::with_timeout(stream, handle, self.timeout, None);
                let (exchange, background) =
                    DnsExchange::connect::<_, _, TokioTime>(multiplexer).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns_split::policy::Racer;
    use crate::dns_split::policy::tests::{FakeUpstream, answer, query};
    use crate::dns_split::server::{DnsSplitHandler, serve_tcp};
    use hickory_proto::rr::{RData, RecordType};
    use std::net::Ipv4Addr;
    use tokio::net::TcpListener;

    const TIMEOUT: Duration = Duration::from_secs(5);

    /// A DNS server over TCP on loopback that answers everything with `ip`.
    async fn tcp_server(ip: Ipv4Addr) -> SocketAddr {
        let upstream = FakeUpstream::new("remote", 0, Some(answer(&[ip])));
        let racer = Racer::new(
            vec![(Group::Upstream, upstream as Arc<dyn DnsUpstream>)],
            TIMEOUT,
            |_| true,
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve_tcp(
            listener,
            Arc::new(DnsSplitHandler::new(racer, None)),
        ));
        addr
    }

    /// Stands in for the cpxy server: records where each connection was for, and connects to
    /// `server` instead.
    struct FakeCpxy {
        server: SocketAddr,
        requests: std::sync::Mutex<Vec<(String, Option<std::net::IpAddr>, u16)>>,
    }

    impl Outbound for FakeCpxy {
        async fn send(
            &self,
            req: OutboundRequest,
        ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static> {
            let ip = match &req.host {
                OutboundHost::Resolved { ip, .. } => *ip,
                OutboundHost::Domain(_) => None,
            };
            self.requests
                .lock()
                .unwrap()
                .push((req.host.host().to_string(), ip, req.port));
            Ok(TcpStream::connect(self.server).await?)
        }
    }

    fn answered_ips(response: &Message) -> Vec<Ipv4Addr> {
        response
            .answers()
            .iter()
            .filter_map(|r| match r.data() {
                RData::A(a) => Some(a.0),
                _ => None,
            })
            .collect()
    }

    async fn ask(spec: &str, cpxy: Option<Arc<dyn Dialer>>) -> anyhow::Result<Vec<Ipv4Addr>> {
        let upstream = HickoryUpstream::new(&spec.parse()?, TIMEOUT, cpxy)?;
        let mut request = query("example.com.", RecordType::A);
        request.set_id(7);
        Ok(answered_ips(&upstream.query(request).await?))
    }

    #[tokio::test]
    async fn queries_via_cpxy_go_through_the_outbound() {
        let ip = Ipv4Addr::new(93, 184, 216, 34);
        let server = tcp_server(ip).await;
        let cpxy = Arc::new(FakeCpxy {
            server,
            requests: Default::default(),
        });
        let dialer: Arc<dyn Dialer> = Arc::new(OutboundDialer(cpxy.clone()));

        assert_eq!(
            ask("tcp://1.1.1.1?via=cpxy", Some(dialer.clone()))
                .await
                .unwrap(),
            vec![ip]
        );
        assert_eq!(
            *cpxy.requests.lock().unwrap(),
            vec![(
                "1.1.1.1".to_string(),
                Some(Ipv4Addr::new(1, 1, 1, 1).into()),
                53
            )]
        );
    }

    #[tokio::test]
    async fn direct_queries_skip_the_outbound() {
        let ip = Ipv4Addr::new(10, 0, 0, 1);
        let server = tcp_server(ip).await;
        let cpxy = Arc::new(FakeCpxy {
            server,
            requests: Default::default(),
        });
        let dialer: Arc<dyn Dialer> = Arc::new(OutboundDialer(cpxy.clone()));

        assert_eq!(
            ask(&format!("tcp://{server}"), Some(dialer)).await.unwrap(),
            vec![ip]
        );
        assert!(cpxy.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn via_cpxy_needs_a_cpxy_server() {
        let error = ask("tcp://1.1.1.1?via=cpxy", None).await.unwrap_err();
        assert!(error.to_string().contains("needs a cpxy server"), "{error}");
    }
}
