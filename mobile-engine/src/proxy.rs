//! Optional LAN proxies, and the app's own loopback proxy. Listener and connection tasks live
//! only as long as the engine.
use anyhow::{Context, ensure};
use client::dns_split::server::DnsSplitHandler;
use client::handshaker::Handshaker;
use client::http_proxy_server::HttpProxyHandshaker;
use client::proxy_handlers::serve;
use client::socks_proxy_server::SocksProxyHandshaker;
use cpxy_ng::outbound::{Outbound, OutboundHost, OutboundRequest};
use hickory_proto::op::{Message, Query};
use hickory_proto::rr::{Name, RData, RecordType};
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

#[derive(Default)]
pub(crate) struct Ports {
    pub http: Option<u16>,
    pub socks: Option<u16>,
    /// An HTTP proxy on a free loopback port for the app's own requests, such as updates: the
    /// app is left out of the VPN so that the engine's sockets bypass it.
    pub local_http: bool,
}

pub(crate) struct Listeners {
    http: Option<TcpListener>,
    socks: Option<TcpListener>,
    local_http: Option<TcpListener>,
}

impl Listeners {
    pub async fn bind(ports: Ports) -> anyhow::Result<Self> {
        async fn bind(port: Option<u16>, name: &str) -> anyhow::Result<Option<TcpListener>> {
            match port {
                Some(port) => Ok(Some(
                    TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
                        .await
                        .with_context(|| format!("Cannot bind {name} proxy on 0.0.0.0:{port}"))?,
                )),
                None => Ok(None),
            }
        }
        let local_http = if ports.local_http {
            Some(
                TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                    .await
                    .context("Cannot bind the local HTTP proxy")?,
            )
        } else {
            None
        };
        Ok(Self {
            http: bind(ports.http, "HTTP").await?,
            socks: bind(ports.socks, "SOCKS5").await?,
            local_http,
        })
    }

    pub fn local_http_port(&self) -> Option<u16> {
        let listener = self.local_http.as_ref()?;
        listener.local_addr().ok().map(|addr| addr.port())
    }

    pub fn spawn<O: Outbound + Send + Sync + 'static>(
        self,
        outbound: Arc<O>,
        dns: Option<Arc<DnsSplitHandler>>,
    ) -> JoinSet<anyhow::Result<()>> {
        let outbound = Arc::new(ProxyOutbound {
            inner: outbound,
            dns,
        });
        let mut tasks = JoinSet::new();
        if let Some(listener) = self.http {
            tasks.spawn(serve_listener::<HttpProxyHandshaker<_>, _>(
                listener,
                outbound.clone(),
            ));
        }
        if let Some(listener) = self.local_http {
            tasks.spawn(serve_listener::<HttpProxyHandshaker<_>, _>(
                listener,
                outbound.clone(),
            ));
        }
        if let Some(listener) = self.socks {
            tasks.spawn(serve_listener::<SocksProxyHandshaker<_>, _>(
                listener, outbound,
            ));
        }
        tasks
    }
}

// Reap completed connections as well as accepting new ones, and cancel active connections
// when this listener is stopped. The shared client's listener does not reap its JoinSet.
async fn serve_listener<HS, O>(listener: TcpListener, outbound: Arc<O>) -> anyhow::Result<()>
where
    HS: Handshaker<TcpStream> + Send + 'static,
    HS::RequestType: Into<OutboundRequest>,
    HS::StreamType: AsyncRead + AsyncWrite + Unpin + Send,
    O: Outbound + Send + Sync + 'static,
{
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                connections.spawn(serve::<HS, _, _>(stream, outbound.clone()));
            }
            Some(result) = connections.join_next() => {
                if !matches!(result, Ok(Ok(()))) {
                    tracing::debug!(?result, "LAN proxy connection ended");
                }
            }
        }
    }
}

/// Resolve proxy hostnames with the VPN's DNS policy before making the IP routing decision.
struct ProxyOutbound<O> {
    inner: Arc<O>,
    dns: Option<Arc<DnsSplitHandler>>,
}

impl<O: Outbound + Send + Sync> Outbound for ProxyOutbound<O> {
    async fn send(
        &self,
        mut req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static> {
        if let OutboundHost::Domain(host) = &req.host {
            let ip = if let Ok(ip) = host.parse::<Ipv4Addr>() {
                Some(ip)
            } else if let Some(dns) = &self.dns {
                let mut query = Message::new();
                query
                    .set_recursion_desired(true)
                    .add_query(Query::query(Name::from_ascii(host)?, RecordType::A));
                let response = dns.handle(&query).await;
                Some(
                    response
                        .answers()
                        .iter()
                        .find_map(|record| match record.data() {
                            RData::A(a) => Some(a.0),
                            _ => None,
                        })
                        .context("Proxy hostname has no IPv4 address")?,
                )
            } else {
                None
            };
            req.host = OutboundHost::Resolved {
                domain: host.clone(),
                ip,
            };
        }
        if let OutboundHost::Resolved { ip: Some(ip), .. } = &req.host {
            ensure!(
                !crate::config::is_sinkhole(*ip),
                "Proxy destination is blocked"
            );
        }
        self.inner.send(req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client::outbound::DirectOutbound;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn relays_http_connect_http_requests_and_socks_and_closes_connections() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let destination = echo.local_addr().unwrap();
            let echo_task = tokio::spawn(async move {
                let mut flows = JoinSet::new();
                loop {
                    tokio::select! {
                        accepted = echo.accept() => {
                            let (mut stream, _) = accepted.unwrap();
                            flows.spawn(async move {
                                let mut bytes = [0; 1024];
                                loop {
                                    let n = stream.read(&mut bytes).await.unwrap();
                                    if n == 0 { break; }
                                    stream.write_all(&bytes[..n]).await.unwrap();
                                }
                            });
                        }
                        Some(_) = flows.join_next() => {}
                    }
                }
            });
            let listeners = Listeners::bind(Ports {
                http: Some(0),
                socks: Some(0),
                local_http: true,
            })
            .await
            .unwrap();
            let http = listeners.http.as_ref().unwrap().local_addr().unwrap();
            let socks = listeners.socks.as_ref().unwrap().local_addr().unwrap();
            let local = listeners.local_http.as_ref().unwrap().local_addr().unwrap();
            assert!(http.ip().is_unspecified());
            assert!(socks.ip().is_unspecified());
            assert!(local.ip().is_loopback());
            assert_eq!(listeners.local_http_port(), Some(local.port()));
            let mut tasks = listeners.spawn(Arc::new(DirectOutbound::default()), None);

            let mut tunnel = TcpStream::connect((Ipv4Addr::LOCALHOST, http.port()))
                .await
                .unwrap();
            tunnel
                .write_all(
                    format!("CONNECT {destination} HTTP/1.1\r\nHost: {destination}\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
            let expected = b"HTTP/1.1 200 Connection Established\r\n\r\n";
            let mut reply = vec![0; expected.len()];
            tunnel.read_exact(&mut reply).await.unwrap();
            assert_eq!(reply, expected);
            tunnel.write_all(b"tunnel payload").await.unwrap();
            let mut payload = [0; 14];
            tunnel.read_exact(&mut payload).await.unwrap();
            assert_eq!(&payload, b"tunnel payload");

            let mut plain = TcpStream::connect((Ipv4Addr::LOCALHOST, http.port()))
                .await
                .unwrap();
            plain
                .write_all(
                    format!(
                        "GET http://{destination}/test HTTP/1.1\r\nHost: {destination}\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(plain.read_u8().await.unwrap());
            }
            assert!(header.starts_with(b"GET /test HTTP/1.1\r\n"));

            let mut own = TcpStream::connect(local).await.unwrap();
            own.write_all(
                format!("CONNECT {destination} HTTP/1.1\r\nHost: {destination}\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
            let mut reply = vec![0; expected.len()];
            own.read_exact(&mut reply).await.unwrap();
            assert_eq!(reply, expected);
            own.write_all(b"own payload").await.unwrap();
            let mut payload = [0; 11];
            own.read_exact(&mut payload).await.unwrap();
            assert_eq!(&payload, b"own payload");

            let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, socks.port()))
                .await
                .unwrap();
            client.write_all(&[5, 1, 0]).await.unwrap();
            let mut greeting = [0; 2];
            client.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 0]);
            // Domain-form request containing an IP also needs to be resolved before routing.
            let mut request = vec![5, 1, 0, 3, 9];
            request.extend_from_slice(b"127.0.0.1");
            request.extend_from_slice(&destination.port().to_be_bytes());
            client.write_all(&request).await.unwrap();
            let mut reply = [0; 10];
            client.read_exact(&mut reply).await.unwrap();
            assert_eq!(reply[1], 0);
            client.write_all(b"socks payload").await.unwrap();
            let mut payload = [0; 13];
            client.read_exact(&mut payload).await.unwrap();
            assert_eq!(&payload, b"socks payload");

            tasks.shutdown().await;
            assert!(
                TcpStream::connect((Ipv4Addr::LOCALHOST, http.port()))
                    .await
                    .is_err()
            );
            assert!(
                TcpStream::connect((Ipv4Addr::LOCALHOST, socks.port()))
                    .await
                    .is_err()
            );
            assert!(TcpStream::connect(local).await.is_err());
            assert_eq!(tunnel.read(&mut payload).await.unwrap(), 0);
            assert_eq!(client.read(&mut payload).await.unwrap(), 0);
            echo_task.abort();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn uses_split_dns_and_rejects_blocked_destinations() {
        use client::dns_split::policy::Racer;
        use client::dns_split::upstream::{DnsUpstream, Group};
        use futures::future::BoxFuture;
        use hickory_proto::op::MessageType;
        use hickory_proto::rr::{Record, rdata::A};
        use std::sync::Mutex;

        struct Answer(Ipv4Addr);
        impl DnsUpstream for Answer {
            fn name(&self) -> &str {
                "test"
            }
            fn query(&self, request: Message) -> BoxFuture<'_, anyhow::Result<Message>> {
                Box::pin(async move {
                    let mut response = Message::new();
                    response
                        .set_message_type(MessageType::Response)
                        .add_queries(request.queries().to_vec());
                    response.add_answer(Record::from_rdata(
                        request.queries()[0].name().clone(),
                        60,
                        RData::A(A(self.0)),
                    ));
                    Ok(response)
                })
            }
        }
        struct Capture(Mutex<Vec<OutboundRequest>>);
        impl Outbound for Capture {
            async fn send(
                &self,
                req: OutboundRequest,
            ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static> {
                self.0.lock().unwrap().push(req);
                Ok(tokio::io::duplex(16).0)
            }
        }
        let inner = Arc::new(Capture(Mutex::new(Vec::new())));
        for ip in [Ipv4Addr::new(223, 5, 5, 5), crate::config::SINKHOLE_ADDR] {
            let dns = DnsSplitHandler::new(
                Racer::new(
                    vec![(Group::Alternative, Arc::new(Answer(ip)))],
                    Duration::from_secs(1),
                    |_| false,
                ),
                None,
            );
            let outbound = ProxyOutbound {
                inner: inner.clone(),
                dns: Some(Arc::new(dns)),
            };
            let request = OutboundRequest {
                host: OutboundHost::Domain("example.com".into()),
                port: 443,
                tls: false,
                initial_plaintext: Vec::new(),
            };
            let result = outbound.send(request).await;
            if crate::config::is_sinkhole(ip) {
                assert!(result.is_err());
            } else {
                assert!(result.is_ok());
                let requests = inner.0.lock().unwrap();
                assert!(
                    matches!(&requests[0].host, OutboundHost::Resolved { domain, ip: Some(resolved) } if domain == "example.com" && *resolved == ip)
                );
            }
        }
        assert_eq!(inner.0.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn occupied_second_port_releases_first_listener() {
        let occupied = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let free = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        let result = Listeners::bind(Ports {
            http: Some(port),
            socks: Some(occupied.local_addr().unwrap().port()),
            local_http: false,
        })
        .await;
        assert!(result.err().unwrap().to_string().contains("SOCKS5"));
        TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
            .await
            .unwrap();
    }
}
