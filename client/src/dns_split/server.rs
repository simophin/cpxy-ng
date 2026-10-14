use super::cache::{DnsCache, unix_now};
use super::policy::Racer;
use super::upstream::DnsUpstream;
use anyhow::Context;
use blocklist_data::Blocklist;
use hickory_proto::op::{Edns, Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::rdata::{A, CNAME};
use hickory_proto::rr::{Name, RData, Record, RecordType};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

/// The EDNS payload size we advertise, as recommended by DNS Flag Day 2020.
const EDNS_PAYLOAD: u16 = 1232;
const TCP_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Short, so turning blocking off soon reaches the clients' own caches.
const SINKHOLE_TTL: u32 = 60;

pub struct DnsSplitHandler {
    racer: Racer,
    cache: Option<Arc<DnsCache>>,
    sinkhole: Option<Sinkhole>,
    local: Option<LocalNames>,
    search_domains: Vec<Name>,
}

/// Names only the local network knows, answered by its own resolver instead of the split.
struct LocalNames {
    server: Arc<dyn DnsUpstream>,
    domains: Vec<Name>,
}

/// Reverse lookups of private (RFC 1918) addresses: only the local resolver knows these hosts.
fn private_reverse_zones() -> impl Iterator<Item = Name> {
    let zones = [
        "10.in-addr.arpa.".to_string(),
        "168.192.in-addr.arpa.".to_string(),
    ];
    zones
        .into_iter()
        .chain((16..=31).map(|octet| format!("{octet}.172.in-addr.arpa.")))
        .map(|zone| Name::from_ascii(zone).expect("valid reverse zone"))
}

/// Answers blocked names with an address that leads nowhere.
struct Sinkhole {
    blocklist: &'static Blocklist,
    addr: Ipv4Addr,
}

impl DnsSplitHandler {
    pub fn new(racer: Racer, cache: Option<Arc<DnsCache>>) -> Self {
        Self {
            racer,
            cache,
            sinkhole: None,
            local: None,
            search_domains: Vec::new(),
        }
    }

    /// Ordered suffixes to try for single-label queries before the original name.
    pub fn with_search_domains(mut self, domains: Vec<Name>) -> Self {
        self.search_domains = domains;
        self
    }

    /// Sends queries for single-label names, names under `domains` and reverse lookups of
    /// private addresses to `server` (the network's own resolver, such as dnsmasq), unsplit
    /// and uncached.
    pub fn with_local(mut self, server: Arc<dyn DnsUpstream>, mut domains: Vec<Name>) -> Self {
        domains.extend(private_reverse_zones());
        self.local = Some(LocalNames { server, domains });
        self
    }

    /// Answers A queries for blocked names with `addr`, and other queries for them with no
    /// records. Whatever carries the traffic is expected to drop packets sent to `addr`.
    pub fn with_sinkhole(mut self, blocklist: &'static Blocklist, addr: Ipv4Addr) -> Self {
        self.sinkhole = Some(Sinkhole { blocklist, addr });
        self
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

        if query.name().num_labels() == 1 {
            for domain in &self.search_domains {
                // Wire-format names are always absolute; only expand single-label names.
                let Ok(name) = query.name().clone().append_domain(domain) else {
                    continue;
                };
                let mut candidate = query.clone();
                candidate.set_name(name.clone());
                let response = self.resolve(request, &candidate).await;
                // A blocked non-A query is deliberately empty; do not search past it.
                if response.response_code() == ResponseCode::NoError
                    && response.answers().is_empty()
                    && self
                        .sinkhole
                        .as_ref()
                        .is_some_and(|s| s.blocklist.is_blocked(&name.to_ascii()))
                    && !self
                        .local
                        .as_ref()
                        .is_some_and(|local| local.matches(&name))
                {
                    return finalize(request, response);
                }
                if response.response_code() == ResponseCode::NoError
                    && !response.answers().is_empty()
                {
                    // Preserve the question and connect its name to the expanded answer.
                    let ttl = response
                        .answers()
                        .iter()
                        .map(Record::ttl)
                        .min()
                        .unwrap_or(0);
                    let mut response = response;
                    response.answers_mut().insert(
                        0,
                        Record::from_rdata(query.name().clone(), ttl, RData::CNAME(CNAME(name))),
                    );
                    response.set_authentic_data(false);
                    return finalize(request, response);
                }
            }
        }
        finalize(request, self.resolve(request, query).await)
    }

    /// Resolve one exact name, so expanded names share routing, blocking and cache policy.
    async fn resolve(&self, request: &Message, query: &Query) -> Message {
        if let Some(local) = &self.local
            && local.matches(query.name())
        {
            return local.resolve(request, query).await;
        }

        if let Some(sinkhole) = &self.sinkhole
            && sinkhole.blocklist.is_blocked(&query.name().to_ascii())
        {
            tracing::info!("Blocked {query}");
            return sinkhole.answer(query);
        }

        if let Some(cache) = &self.cache {
            match cache.get(query, unix_now()).await {
                Ok(Some(cached)) => {
                    tracing::debug!("Cache hit for {query}");
                    return cached;
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("Error reading DNS cache: {e:?}"),
            }
        }

        let upstream_request = upstream_request(request, query);
        let Some(resolution) = self.racer.resolve(&upstream_request).await else {
            tracing::info!("No DNS server answered {query}");
            return error(ResponseCode::ServFail);
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

        resolution.message
    }
}

impl LocalNames {
    fn matches(&self, name: &Name) -> bool {
        name.num_labels() == 1 || self.domains.iter().any(|domain| domain.zone_of(name))
    }

    async fn resolve(&self, request: &Message, query: &Query) -> Message {
        match self.server.query(upstream_request(request, query)).await {
            Ok(message) => {
                tracing::info!(
                    "Answering {query} with {} from local server",
                    message.response_code()
                );
                message
            }
            Err(e) => {
                tracing::info!("Local server failed for {query}: {e:?}");
                error(ResponseCode::ServFail)
            }
        }
    }
}

impl Sinkhole {
    fn answer(&self, query: &Query) -> Message {
        let mut message = error(ResponseCode::NoError);
        if query.query_type() == RecordType::A {
            message.add_answer(Record::from_rdata(
                query.name().clone(),
                SINKHOLE_TTL,
                RData::A(A(self.addr)),
            ));
        }
        message
    }
}

/// The query sent on for a client's request.
fn upstream_request(request: &Message, query: &Query) -> Message {
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
    upstream_request
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

    /// Records exact names and gives each one its configured response.
    struct SearchUpstream {
        calls: std::sync::Mutex<Vec<String>>,
        answers: std::collections::HashMap<String, Message>,
    }

    impl DnsUpstream for SearchUpstream {
        fn name(&self) -> &str {
            "search-test"
        }
        fn query(
            &self,
            request: Message,
        ) -> futures::future::BoxFuture<'_, anyhow::Result<Message>> {
            Box::pin(async move {
                let name = request.queries()[0].name().to_ascii();
                self.calls.lock().unwrap().push(name.clone());
                Ok(self
                    .answers
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| error(ResponseCode::NXDomain)))
            })
        }
    }

    #[tokio::test]
    async fn search_is_ordered_cached_and_preserves_the_question() {
        let name = Name::from_ascii("printer.second.example.").unwrap();
        let mut positive = error(ResponseCode::NoError);
        positive.add_answer(Record::from_rdata(
            name.clone(),
            90,
            RData::A(A(Ipv4Addr::new(10, 0, 0, 1))),
        ));
        let upstream = Arc::new(SearchUpstream {
            calls: Default::default(),
            answers: [(name.to_ascii(), positive)].into(),
        });
        let cache = Arc::new(DnsCache::open_in_memory(OPTIONS).await.unwrap());
        let handler = DnsSplitHandler::new(
            Racer::new(
                vec![(Group::Alternative, upstream.clone())],
                Duration::from_secs(1),
                local_ip,
            ),
            Some(cache.clone()),
        )
        .with_search_domains(
            ["first.example.", "second.example.", "third.example."]
                .into_iter()
                .map(|s| Name::from_ascii(s).unwrap())
                .collect(),
        );
        let req = request("printer.", RecordType::A);
        for _ in 0..2 {
            let response = handler.handle(&req).await;
            assert_eq!(response.queries(), req.queries());
            assert_eq!(response.id(), req.id());
            assert!(!response.authentic_data());
            assert_eq!(response.answers()[0].name(), req.queries()[0].name());
            assert_eq!(
                response.answers()[0].data(),
                &RData::CNAME(CNAME(name.clone()))
            );
            assert_eq!(response.answers()[1].name(), &name);
            assert!(Message::from_vec(&encode_udp(&req, &response).unwrap()).is_ok());
        }
        assert_eq!(
            *upstream.calls.lock().unwrap(),
            ["printer.first.example.", "printer.second.example."]
        );
        assert!(
            cache
                .get(&req.queries()[0], unix_now())
                .await
                .unwrap()
                .is_none()
        );
        let response = handler
            .handle(&request("www.example.com.", RecordType::A))
            .await;
        assert_eq!(response.response_code(), ResponseCode::NXDomain);
        assert_eq!(
            upstream.calls.lock().unwrap().last().unwrap(),
            "www.example.com."
        );
    }

    #[tokio::test]
    async fn search_falls_back_after_negative_empty_and_failed_answers() {
        let upstream = Arc::new(SearchUpstream {
            calls: Default::default(),
            answers: [
                ("host.empty.example.".into(), error(ResponseCode::NoError)),
                ("host.failed.example.".into(), error(ResponseCode::ServFail)),
            ]
            .into(),
        });
        let handler = DnsSplitHandler::new(
            Racer::new(
                vec![(Group::Alternative, upstream.clone())],
                Duration::from_secs(1),
                local_ip,
            ),
            None,
        )
        .with_search_domains(
            ["missing.example.", "empty.example.", "failed.example."]
                .into_iter()
                .map(|s| Name::from_ascii(s).unwrap())
                .collect(),
        );
        let response = handler.handle(&request("host.", RecordType::A)).await;
        assert_eq!(response.response_code(), ResponseCode::NXDomain);
        assert_eq!(
            *upstream.calls.lock().unwrap(),
            [
                "host.missing.example.",
                "host.empty.example.",
                "host.failed.example.",
                "host."
            ]
        );
        upstream.calls.lock().unwrap().clear();
        handler.handle(&request("host.", RecordType::AAAA)).await;
        assert!(upstream.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn searched_names_obey_local_routing_and_blocking() {
        let upstream = FakeUpstream::new("u", 0, None);
        let local = FakeUpstream::new("l", 0, Some(answer(&[Ipv4Addr::new(192, 168, 1, 2)])));
        let (handler, _) = handler(&[(Group::Upstream, upstream.clone())]).await;
        let domain = Name::from_ascii("lan.").unwrap();
        let handler = handler
            .with_local(local.clone(), vec![domain.clone()])
            .with_search_domains(vec![domain]);
        assert_eq!(
            handler
                .handle(&request("printer.", RecordType::A))
                .await
                .answers()
                .len(),
            2
        );
        assert_eq!(local.calls.load(Ordering::SeqCst), 1);
        assert_eq!(upstream.calls.load(Ordering::SeqCst), 0);

        let (handler, _) = self::handler(&[(Group::Upstream, upstream.clone())]).await;
        let blocklist = Box::leak(Box::new(
            Blocklist::from_list("||ads.example.com^").unwrap(),
        ));
        let handler = handler
            .with_search_domains(vec![Name::from_ascii("example.com.").unwrap()])
            .with_sinkhole(blocklist, Ipv4Addr::new(198, 18, 0, 1));
        let response = handler.handle(&request("ads.", RecordType::A)).await;
        assert_eq!(response.answers().len(), 2);
        let response = handler.handle(&request("ads.", RecordType::HTTPS)).await;
        assert_eq!(response.response_code(), ResponseCode::NoError);
        assert!(response.answers().is_empty());
        assert_eq!(upstream.calls.load(Ordering::SeqCst), 0);
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
    async fn blocked_names_are_sinkholed_without_querying() {
        let upstream = FakeUpstream::new("u", 0, Some(answer(&[Ipv4Addr::new(1, 2, 3, 4)])));
        let (handler, _) = handler(&[(Group::Upstream, upstream.clone())]).await;
        let blocklist = Box::leak(Box::new(
            Blocklist::from_list("||ads.example.com^").unwrap(),
        ));
        let sinkhole = Ipv4Addr::new(198, 18, 0, 1);
        let handler = handler.with_sinkhole(blocklist, sinkhole);

        let response = handler
            .handle(&request("x.ads.example.com.", RecordType::A))
            .await;
        assert_eq!(response.id(), 4242);
        assert_eq!(response.response_code(), ResponseCode::NoError);
        let [record] = response.answers() else {
            panic!("expected one answer, got {:?}", response.answers())
        };
        assert_eq!(record.data(), &RData::A(A(sinkhole)));

        let response = handler
            .handle(&request("ads.example.com.", RecordType::HTTPS))
            .await;
        assert_eq!(response.response_code(), ResponseCode::NoError);
        assert!(response.answers().is_empty());
        assert_eq!(upstream.calls.load(Ordering::SeqCst), 0);

        // Other names still resolve
        let response = handler
            .handle(&request("example.com.", RecordType::A))
            .await;
        assert_eq!(response.answers().len(), 1);
        assert_eq!(upstream.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn local_names_go_to_the_local_server_only() {
        let upstream = FakeUpstream::new("u", 0, Some(answer(&[Ipv4Addr::new(10, 0, 0, 1)])));
        let local = FakeUpstream::new("l", 0, Some(answer(&[Ipv4Addr::new(192, 168, 7, 20)])));
        let (handler, cache) = handler(&[(Group::Upstream, upstream.clone())]).await;
        let handler = handler.with_local(local.clone(), vec![Name::from_ascii("lan.").unwrap()]);

        let local_names = [
            "printer.",
            "printer.lan.",
            "lan.",
            "20.7.168.192.in-addr.arpa.",
            "1.0.0.10.in-addr.arpa.",
            "9.0.20.172.in-addr.arpa.",
        ];
        for name in local_names {
            let req = request(name, RecordType::A);
            let response = handler.handle(&req).await;
            assert_eq!(response.id(), 4242, "{name}");
            assert_eq!(response.answers().len(), 1, "{name}");
            let cached = cache.get(&req.queries()[0], unix_now()).await.unwrap();
            assert!(cached.is_none(), "{name} was cached");
        }
        assert_eq!(local.calls.load(Ordering::SeqCst), local_names.len());
        assert_eq!(upstream.calls.load(Ordering::SeqCst), 0);

        // Everything else is split as usual
        let public_names = [
            "example.com.",
            "lan.example.com.",
            "1.0.0.8.in-addr.arpa.",
            "9.0.32.172.in-addr.arpa.",
        ];
        for name in public_names {
            handler.handle(&request(name, RecordType::A)).await;
        }
        assert_eq!(local.calls.load(Ordering::SeqCst), local_names.len());
        assert_eq!(upstream.calls.load(Ordering::SeqCst), public_names.len());
    }

    #[tokio::test]
    async fn local_server_failure_is_servfail() {
        let upstream = FakeUpstream::new("u", 0, Some(answer(&[Ipv4Addr::new(10, 0, 0, 1)])));
        let (handler, _) = handler(&[(Group::Upstream, upstream.clone())]).await;
        let handler = handler.with_local(FakeUpstream::new("l", 0, None), vec![]);

        let response = handler.handle(&request("nas.", RecordType::A)).await;
        assert_eq!(response.response_code(), ResponseCode::ServFail);
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
