use super::upstream::{DnsUpstream, Group};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use hickory_proto::op::{Message, ResponseCode};
use hickory_proto::rr::{RData, Record};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

/// The answer chosen for a query.
#[derive(Debug)]
pub struct Resolution {
    pub message: Message,
    pub source: Group,
    /// False for the last-resort upstream answer used when no alternative server answered.
    pub cacheable: bool,
}

/// Sends each query to every configured server and picks an answer:
///
/// - The first answer with any address in the local region wins, from either group, with its
///   addresses outside the local region removed.
/// - Otherwise the first answer from an alternative server wins.
/// - If no alternative server answers, the first upstream answer is used as a last resort.
pub struct Racer {
    servers: Vec<(Group, Arc<dyn DnsUpstream>)>,
    timeout: Duration,
    is_local_ip: fn(Ipv4Addr) -> bool,
}

impl Racer {
    pub fn new(
        servers: Vec<(Group, Arc<dyn DnsUpstream>)>,
        timeout: Duration,
        is_local_ip: fn(Ipv4Addr) -> bool,
    ) -> Self {
        Self {
            servers,
            timeout,
            is_local_ip,
        }
    }

    pub async fn resolve(&self, request: &Message) -> Option<Resolution> {
        let mut pending: FuturesUnordered<_> = self
            .servers
            .iter()
            .map(|(group, server)| {
                let request = request.clone();
                async move { (*group, server.name(), server.query(request).await) }
            })
            .collect();

        let mut fallback = None;
        let deadline = tokio::time::sleep(self.timeout);
        tokio::pin!(deadline);

        loop {
            let (group, name, result) = tokio::select! {
                next = pending.next() => match next {
                    Some(next) => next,
                    None => break,
                },
                _ = &mut deadline => {
                    tracing::debug!("Timed out waiting for DNS servers");
                    break;
                }
            };

            let mut message = match result {
                Ok(m) if is_usable(&m) => m,
                Ok(m) => {
                    tracing::debug!("{group} server {name} returned {}", m.response_code());
                    continue;
                }
                Err(e) => {
                    tracing::debug!("{group} server {name} failed: {e:?}");
                    continue;
                }
            };

            if keep_local_region_answers(&mut message, self.is_local_ip) {
                tracing::debug!("Using local-region answer from {group} server {name}");
                return Some(Resolution {
                    message,
                    source: group,
                    cacheable: true,
                });
            }

            match group {
                Group::Alternative => {
                    tracing::debug!("Using answer from alternative server {name}");
                    return Some(Resolution {
                        message,
                        source: group,
                        cacheable: true,
                    });
                }
                Group::Upstream => {
                    tracing::debug!("Discarding non-local-region answer from upstream {name}");
                    fallback.get_or_insert(message);
                }
            }
        }

        fallback.map(|message| Resolution {
            message,
            source: Group::Upstream,
            cacheable: false,
        })
    }
}

fn is_usable(message: &Message) -> bool {
    matches!(
        message.response_code(),
        ResponseCode::NoError | ResponseCode::NXDomain
    )
}

/// True when the answer has at least one A record in the local region, in which case the A records
/// outside it are removed. Leaves the message untouched when it returns false.
pub fn keep_local_region_answers(message: &mut Message, is_local_ip: fn(Ipv4Addr) -> bool) -> bool {
    if message.response_code() != ResponseCode::NoError {
        return false;
    }

    let is_local_record = |r: &Record| matches!(r.data(), RData::A(a) if is_local_ip(a.0));
    if !message.answers().iter().any(is_local_record) {
        return false;
    }

    message
        .answers_mut()
        .retain(|r| !matches!(r.data(), RData::A(_)) || is_local_record(r));
    let count = message.answers().len() as u16;
    message.set_answer_count(count);
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use hickory_proto::op::{MessageType, Query};
    use hickory_proto::rr::rdata::{A, CNAME};
    use hickory_proto::rr::{Name, RecordType};
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub fn local_ip(ip: Ipv4Addr) -> bool {
        ip.octets()[0] == 10
    }

    pub fn query(name: &str, record_type: RecordType) -> Message {
        let mut message = Message::new();
        message.add_query(Query::query(Name::from_ascii(name).unwrap(), record_type));
        message.set_recursion_desired(true);
        message
    }

    pub fn answer(ips: &[Ipv4Addr]) -> Message {
        let mut message = query("example.com.", RecordType::A);
        message.set_message_type(MessageType::Response);
        for ip in ips {
            message.add_answer(Record::from_rdata(
                Name::from_ascii("example.com.").unwrap(),
                300,
                RData::A(A(*ip)),
            ));
        }
        message
    }

    pub struct FakeUpstream {
        pub name: String,
        pub delay: Duration,
        pub response: Option<Message>,
        pub calls: AtomicUsize,
    }

    impl FakeUpstream {
        pub fn new(name: &str, delay_ms: u64, response: Option<Message>) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                delay: Duration::from_millis(delay_ms),
                response,
                calls: AtomicUsize::new(0),
            })
        }
    }

    impl DnsUpstream for FakeUpstream {
        fn name(&self) -> &str {
            &self.name
        }

        fn query(&self, _request: Message) -> BoxFuture<'_, anyhow::Result<Message>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                tokio::time::sleep(self.delay).await;
                self.response
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("{} failed", self.name))
            })
        }
    }

    fn racer(servers: Vec<(Group, Arc<FakeUpstream>)>) -> Racer {
        Racer::new(
            servers
                .into_iter()
                .map(|(g, s)| (g, s as Arc<dyn DnsUpstream>))
                .collect(),
            Duration::from_secs(5),
            local_ip,
        )
    }

    fn first_ip(message: &Message) -> Ipv4Addr {
        match message.answers()[0].data() {
            RData::A(a) => a.0,
            _ => panic!("Not an A record"),
        }
    }

    const LOCAL: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);
    const LOCAL_2: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 2);
    const REMOTE: Ipv4Addr = Ipv4Addr::new(8, 8, 8, 8);
    const REMOTE_2: Ipv4Addr = Ipv4Addr::new(8, 8, 4, 4);

    #[tokio::test(start_paused = true)]
    async fn local_upstream_answer_wins_immediately() {
        let racer = racer(vec![
            (
                Group::Upstream,
                FakeUpstream::new("u", 10, Some(answer(&[LOCAL]))),
            ),
            (
                Group::Alternative,
                FakeUpstream::new("a", 100, Some(answer(&[REMOTE]))),
            ),
        ]);
        let r = racer
            .resolve(&query("example.com.", RecordType::A))
            .await
            .unwrap();
        assert_eq!(r.source, Group::Upstream);
        assert!(r.cacheable);
        assert_eq!(first_ip(&r.message), LOCAL);
    }

    #[tokio::test(start_paused = true)]
    async fn non_local_upstream_answer_is_discarded() {
        let racer = racer(vec![
            (
                Group::Upstream,
                FakeUpstream::new("u", 10, Some(answer(&[REMOTE]))),
            ),
            (
                Group::Alternative,
                FakeUpstream::new("a", 100, Some(answer(&[REMOTE_2]))),
            ),
        ]);
        let r = racer
            .resolve(&query("example.com.", RecordType::A))
            .await
            .unwrap();
        assert_eq!(r.source, Group::Alternative);
        assert_eq!(first_ip(&r.message), REMOTE_2);
    }

    fn a_records(message: &Message) -> Vec<Ipv4Addr> {
        message
            .answers()
            .iter()
            .filter_map(|r| match r.data() {
                RData::A(a) => Some(a.0),
                _ => None,
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn mixed_answer_keeps_only_local_addresses() {
        let racer = racer(vec![
            (
                Group::Upstream,
                FakeUpstream::new("u", 10, Some(answer(&[REMOTE, LOCAL, REMOTE_2, LOCAL_2]))),
            ),
            (
                Group::Alternative,
                FakeUpstream::new("a", 100, Some(answer(&[REMOTE_2]))),
            ),
        ]);
        let r = racer
            .resolve(&query("example.com.", RecordType::A))
            .await
            .unwrap();
        assert_eq!(r.source, Group::Upstream);
        assert!(r.cacheable);
        assert_eq!(a_records(&r.message), vec![LOCAL, LOCAL_2]);
        assert_eq!(r.message.answer_count(), 2);
    }

    #[test]
    fn filtering_keeps_cname_records() {
        let mut message = query("www.example.com.", RecordType::A);
        message.set_message_type(MessageType::Response);
        message.add_answer(Record::from_rdata(
            Name::from_ascii("www.example.com.").unwrap(),
            300,
            RData::CNAME(CNAME(Name::from_ascii("example.com.").unwrap())),
        ));
        for record in answer(&[REMOTE, LOCAL]).take_answers() {
            message.add_answer(record);
        }

        assert!(keep_local_region_answers(&mut message, local_ip));
        assert!(matches!(message.answers()[0].data(), RData::CNAME(_)));
        assert_eq!(a_records(&message), vec![LOCAL]);
    }

    #[test]
    fn non_local_answer_is_left_untouched() {
        let mut message = answer(&[REMOTE, REMOTE_2]);
        assert!(!keep_local_region_answers(&mut message, local_ip));
        assert_eq!(a_records(&message), vec![REMOTE, REMOTE_2]);
    }

    #[tokio::test(start_paused = true)]
    async fn alternative_answer_first_wins() {
        let racer = racer(vec![
            (
                Group::Upstream,
                FakeUpstream::new("u", 100, Some(answer(&[LOCAL]))),
            ),
            (
                Group::Alternative,
                FakeUpstream::new("a", 10, Some(answer(&[REMOTE]))),
            ),
        ]);
        let r = racer
            .resolve(&query("example.com.", RecordType::A))
            .await
            .unwrap();
        assert_eq!(r.source, Group::Alternative);
        assert_eq!(first_ip(&r.message), REMOTE);
    }

    #[tokio::test(start_paused = true)]
    async fn local_alternative_answer_wins() {
        let racer = racer(vec![
            (
                Group::Upstream,
                FakeUpstream::new("u", 100, Some(answer(&[REMOTE]))),
            ),
            (
                Group::Alternative,
                FakeUpstream::new("a", 10, Some(answer(&[LOCAL_2]))),
            ),
        ]);
        let r = racer
            .resolve(&query("example.com.", RecordType::A))
            .await
            .unwrap();
        assert_eq!(r.source, Group::Alternative);
        assert_eq!(first_ip(&r.message), LOCAL_2);
    }

    #[tokio::test(start_paused = true)]
    async fn empty_upstream_answer_waits_for_alternative() {
        let racer = racer(vec![
            (
                Group::Upstream,
                FakeUpstream::new("u", 10, Some(answer(&[]))),
            ),
            (
                Group::Alternative,
                FakeUpstream::new("a", 100, Some(answer(&[REMOTE]))),
            ),
        ]);
        let r = racer
            .resolve(&query("example.com.", RecordType::A))
            .await
            .unwrap();
        assert_eq!(r.source, Group::Alternative);
    }

    #[tokio::test(start_paused = true)]
    async fn falls_back_to_upstream_when_alternatives_fail() {
        let racer = racer(vec![
            (
                Group::Upstream,
                FakeUpstream::new("u", 10, Some(answer(&[REMOTE]))),
            ),
            (Group::Alternative, FakeUpstream::new("a", 20, None)),
        ]);
        let r = racer
            .resolve(&query("example.com.", RecordType::A))
            .await
            .unwrap();
        assert_eq!(r.source, Group::Upstream);
        assert!(!r.cacheable);
        assert_eq!(first_ip(&r.message), REMOTE);
    }

    #[tokio::test(start_paused = true)]
    async fn falls_back_to_upstream_on_timeout() {
        let racer = racer(vec![
            (
                Group::Upstream,
                FakeUpstream::new("u", 10, Some(answer(&[REMOTE]))),
            ),
            (
                Group::Alternative,
                FakeUpstream::new("a", 60_000, Some(answer(&[REMOTE_2]))),
            ),
        ]);
        let r = racer
            .resolve(&query("example.com.", RecordType::A))
            .await
            .unwrap();
        assert_eq!(r.source, Group::Upstream);
        assert!(!r.cacheable);
    }

    #[tokio::test(start_paused = true)]
    async fn servfail_is_ignored() {
        let mut servfail = answer(&[]);
        servfail.set_response_code(ResponseCode::ServFail);
        let racer = racer(vec![
            (
                Group::Upstream,
                FakeUpstream::new("u", 100, Some(answer(&[LOCAL]))),
            ),
            (
                Group::Alternative,
                FakeUpstream::new("a", 10, Some(servfail)),
            ),
        ]);
        let r = racer
            .resolve(&query("example.com.", RecordType::A))
            .await
            .unwrap();
        assert_eq!(r.source, Group::Upstream);
        assert!(r.cacheable);
    }

    #[tokio::test(start_paused = true)]
    async fn returns_none_when_everything_fails() {
        let racer = racer(vec![
            (Group::Upstream, FakeUpstream::new("u", 10, None)),
            (Group::Alternative, FakeUpstream::new("a", 20, None)),
        ]);
        assert!(
            racer
                .resolve(&query("example.com.", RecordType::A))
                .await
                .is_none()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn queries_every_server() {
        let servers = vec![
            (
                Group::Upstream,
                FakeUpstream::new("u1", 10, Some(answer(&[LOCAL]))),
            ),
            (Group::Upstream, FakeUpstream::new("u2", 10, None)),
            (Group::Alternative, FakeUpstream::new("a1", 10, None)),
            (Group::Alternative, FakeUpstream::new("a2", 10, None)),
        ];
        let racer = racer(servers.clone());
        racer.resolve(&query("example.com.", RecordType::A)).await;
        for (_, s) in servers {
            assert_eq!(s.calls.load(Ordering::SeqCst), 1, "{}", s.name);
        }
    }
}
