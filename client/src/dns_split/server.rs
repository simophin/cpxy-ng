use super::cache::{DnsCache, unix_now};
use super::policy::Racer;
use anyhow::Context;
use hickory_proto::op::{Edns, Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::RecordType;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

/// The EDNS payload size we advertise, as recommended by DNS Flag Day 2020.
const EDNS_PAYLOAD: u16 = 1232;
const TCP_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct DnsSplitHandler {
    racer: Racer,
    cache: Option<Arc<DnsCache>>,
}

impl DnsSplitHandler {
    pub fn new(racer: Racer, cache: Option<Arc<DnsCache>>) -> Self {
        Self { racer, cache }
    }

    pub async fn handle(&self, request: &Message) -> Message {
        if request.message_type() != MessageType::Query || request.op_code() != OpCode::Query {
            return finalize(request, error(ResponseCode::NotImp));
        }

        let [query] = request.queries() else {
            return finalize(request, error(ResponseCode::FormErr));
        };

        // IPv6 is not supported: answer AAAA with no records so clients fall back to IPv4.
        if query.query_type() == RecordType::AAAA {
            return finalize(request, error(ResponseCode::NoError));
        }

        if let Some(cache) = &self.cache {
            match cache.get(query, unix_now()).await {
                Ok(Some(cached)) => {
                    tracing::debug!("Cache hit for {query}");
                    return finalize(request, cached);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("Error reading DNS cache: {e:?}"),
            }
        }

        let mut upstream_request = Message::new();
        upstream_request
            .set_message_type(MessageType::Query)
            .set_op_code(OpCode::Query)
            .set_recursion_desired(true)
            .set_checking_disabled(request.checking_disabled())
            .add_query(query.clone());
        let mut edns = Edns::new();
        edns.set_max_payload(EDNS_PAYLOAD);
        upstream_request.set_edns(edns);

        let Some(resolution) = self.racer.resolve(&upstream_request).await else {
            tracing::info!("No DNS server answered {query}");
            return finalize(request, error(ResponseCode::ServFail));
        };

        tracing::info!(
            "Answering {query} with {} from {} server",
            resolution.message.response_code(),
            resolution.source
        );

        if resolution.cacheable
            && let Some(cache) = &self.cache
            && let Err(e) = cache
                .put(query, &resolution.message, resolution.source, unix_now())
                .await
        {
            tracing::warn!("Error writing DNS cache: {e:?}");
        }

        finalize(request, resolution.message)
    }
}

fn error(code: ResponseCode) -> Message {
    let mut message = Message::new();
    message.set_response_code(code);
    message
}

/// Turns an answer into the reply for this particular request.
fn finalize(request: &Message, mut response: Message) -> Message {
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .set_op_code(request.op_code())
        .set_authoritative(false)
        .set_truncated(false)
        .set_recursion_desired(request.recursion_desired())
        .set_recursion_available(true);
    *response.queries_mut() = request.queries().to_vec();
    *response.extensions_mut() = request.extensions().as_ref().map(|_| {
        let mut edns = Edns::new();
        edns.set_max_payload(EDNS_PAYLOAD);
        edns
    });
    response
}

fn encode(response: &Message) -> anyhow::Result<Vec<u8>> {
    response.to_vec().context("Error encoding DNS response")
}

/// Encodes a UDP reply, truncating it when it is larger than the client accepts.
pub fn encode_udp(request: &Message, response: &Message) -> anyhow::Result<Vec<u8>> {
    let bytes = encode(response)?;
    let limit = usize::from(request.max_payload().min(EDNS_PAYLOAD));
    if bytes.len() <= limit {
        return Ok(bytes);
    }
    encode(&response.truncate())
}

pub async fn serve_udp(socket: UdpSocket, handler: Arc<DnsSplitHandler>) -> anyhow::Result<()> {
    let socket = Arc::new(socket);
    let mut buf = vec![0u8; 65535];
    loop {
        let (len, peer) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Error receiving DNS request over UDP: {e:?}");
                continue;
            }
        };

        let request = match Message::from_vec(&buf[..len]) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("Invalid DNS request from {peer}: {e:?}");
                continue;
            }
        };

        let socket = socket.clone();
        let handler = handler.clone();
        tokio::spawn(async move {
            let response = handler.handle(&request).await;
            match encode_udp(&request, &response) {
                Ok(bytes) => {
                    if let Err(e) = socket.send_to(&bytes, peer).await {
                        tracing::debug!("Error replying to {peer}: {e:?}");
                    }
                }
                Err(e) => tracing::warn!("{e:?}"),
            }
        });
    }
}

pub async fn serve_tcp(listener: TcpListener, handler: Arc<DnsSplitHandler>) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = listener
            .accept()
            .await
            .context("Error accepting DNS connection")?;
        let handler = handler.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_tcp_connection(stream, &handler).await {
                tracing::debug!("DNS connection from {peer} ended: {e:?}");
            }
        });
    }
}

/// Answers length-prefixed DNS queries on one TCP connection until it goes idle or closes.
pub async fn serve_tcp_connection(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    handler: &DnsSplitHandler,
) -> anyhow::Result<()> {
    loop {
        let len = match tokio::time::timeout(TCP_IDLE_TIMEOUT, stream.read_u16()).await {
            Ok(Ok(len)) => len,
            // Idle timeout or the client closed the connection.
            Ok(Err(_)) | Err(_) => return Ok(()),
        };

        let mut buf = vec![0u8; usize::from(len)];
        stream.read_exact(&mut buf).await?;
        let request = Message::from_vec(&buf).context("Invalid DNS request")?;
        let bytes = encode(&handler.handle(&request).await)?;
        let len = u16::try_from(bytes.len()).context("DNS response too large")?;

        stream.write_u16(len).await?;
        stream.write_all(&bytes).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns_split::cache::CacheOptions;
    use crate::dns_split::policy::tests::{FakeUpstream, answer, local_ip, query};
    use crate::dns_split::upstream::{DnsUpstream, Group};
    use std::net::Ipv4Addr;
    use std::sync::atomic::Ordering;

    const OPTIONS: CacheOptions = CacheOptions {
        min_ttl: 30,
        max_ttl: 3600,
        negative_ttl: 300,
    };

    async fn handler(servers: &[(Group, Arc<FakeUpstream>)]) -> (DnsSplitHandler, Arc<DnsCache>) {
        let racer = Racer::new(
            servers
                .iter()
                .map(|(g, s)| (*g, s.clone() as Arc<dyn DnsUpstream>))
                .collect(),
            Duration::from_secs(5),
            local_ip,
        );
        let cache = Arc::new(DnsCache::open_in_memory(OPTIONS).await.unwrap());
        (DnsSplitHandler::new(racer, Some(cache.clone())), cache)
    }

    fn request(name: &str, record_type: RecordType) -> Message {
        let mut request = query(name, record_type);
        request.set_id(4242);
        request
    }

    #[tokio::test]
    async fn aaaa_is_answered_without_querying() {
        let upstream = FakeUpstream::new("u", 0, Some(answer(&[])));
        let (handler, _) = handler(&[(Group::Upstream, upstream.clone())]).await;

        let response = handler
            .handle(&request("example.com.", RecordType::AAAA))
            .await;
        assert_eq!(response.id(), 4242);
        assert_eq!(response.response_code(), ResponseCode::NoError);
        assert!(response.answers().is_empty());
        assert_eq!(upstream.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn answers_are_cached() {
        let alternative = FakeUpstream::new("a", 0, Some(answer(&[Ipv4Addr::new(8, 8, 8, 8)])));
        let (handler, _) = handler(&[(Group::Alternative, alternative.clone())]).await;

        let first = handler
            .handle(&request("example.com.", RecordType::A))
            .await;
        let second = handler
            .handle(&request("example.com.", RecordType::A))
            .await;
        assert_eq!(first.answers(), second.answers());
        assert_eq!(second.id(), 4242);
        assert_eq!(alternative.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fallback_answers_are_not_cached() {
        let upstream = FakeUpstream::new("u", 0, Some(answer(&[Ipv4Addr::new(8, 8, 8, 8)])));
        let alternative = FakeUpstream::new("a", 0, None);
        let (handler, cache) = handler(&[
            (Group::Upstream, upstream.clone()),
            (Group::Alternative, alternative),
        ])
        .await;

        let req = request("example.com.", RecordType::A);
        let response = handler.handle(&req).await;
        assert_eq!(response.answers().len(), 1);
        assert!(
            cache
                .get(&req.queries()[0], unix_now())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn servfail_when_nothing_answers() {
        let (handler, _) = handler(&[(Group::Alternative, FakeUpstream::new("a", 0, None))]).await;
        let response = handler
            .handle(&request("example.com.", RecordType::A))
            .await;
        assert_eq!(response.response_code(), ResponseCode::ServFail);
    }

    #[test]
    fn large_udp_replies_are_truncated() {
        let req = request("example.com.", RecordType::A);
        let ips: Vec<_> = (0..100).map(|i| Ipv4Addr::new(10, 0, 0, i)).collect();
        let response = finalize(&req, answer(&ips));

        let bytes = encode_udp(&req, &response).unwrap();
        assert!(bytes.len() <= 512);
        let decoded = Message::from_vec(&bytes).unwrap();
        assert!(decoded.truncated());
        assert_eq!(decoded.id(), 4242);
    }
}
