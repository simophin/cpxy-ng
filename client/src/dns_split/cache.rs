use super::upstream::Group;
use anyhow::Context;
use hickory_proto::op::{Message, Query, ResponseCode};
use hickory_proto::rr::{RData, Record};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy)]
pub struct CacheOptions {
    /// Lower bound for how long a positive answer is kept.
    pub min_ttl: u32,
    /// Upper bound for how long a positive answer is kept.
    pub max_ttl: u32,
    /// Upper bound for how long a negative answer (NXDOMAIN or no records) is kept.
    pub negative_ttl: u32,
}

/// A persistent cache of chosen answers, keyed by question.
pub struct DnsCache {
    pool: SqlitePool,
    options: CacheOptions,
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

impl DnsCache {
    pub async fn open(path: &Path, options: CacheOptions) -> anyhow::Result<Self> {
        let connect_options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(connect_options)
            .await
            .with_context(|| format!("Error opening DNS cache {}", path.display()))?;

        Self::from_pool(pool, options).await
    }

    pub async fn open_in_memory(options: CacheOptions) -> anyhow::Result<Self> {
        // Each in-memory connection is a separate database, so keep exactly one alive.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(SqliteConnectOptions::new().in_memory(true))
            .await?;

        Self::from_pool(pool, options).await
    }

    async fn from_pool(pool: SqlitePool, options: CacheOptions) -> anyhow::Result<Self> {
        sqlx::migrate!("./migrations/dns_split")
            .run(&pool)
            .await
            .context("Error migrating DNS cache")?;
        Ok(Self { pool, options })
    }

    /// Returns the cached answer for the question, with TTLs lowered to the time it has left.
    pub async fn get(&self, query: &Query, now: i64) -> anyhow::Result<Option<Message>> {
        let row: Option<(Vec<u8>, i64)> = sqlx::query_as(
            "SELECT response, expires_at FROM dns_cache \
             WHERE name = ? AND qtype = ? AND qclass = ? AND expires_at > ?",
        )
        .bind(cache_name(query))
        .bind(u16::from(query.query_type()))
        .bind(u16::from(query.query_class()))
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;

        let Some((response, expires_at)) = row else {
            return Ok(None);
        };

        let mut message = Message::from_vec(&response).context("Error decoding cached answer")?;
        let remaining = u32::try_from(expires_at - now).unwrap_or(u32::MAX);
        let count_down = |records: &mut Vec<Record>| {
            for record in records {
                record.set_ttl(record.ttl().min(remaining));
            }
        };
        count_down(message.answers_mut());
        count_down(message.name_servers_mut());
        count_down(message.additionals_mut());

        Ok(Some(message))
    }

    /// Stores the answer if it is cacheable. Returns whether it was stored.
    pub async fn put(
        &self,
        query: &Query,
        message: &Message,
        source: Group,
        now: i64,
    ) -> anyhow::Result<bool> {
        let Some(ttl) = cache_ttl(message, &self.options) else {
            return Ok(false);
        };

        sqlx::query(
            "INSERT INTO dns_cache (name, qtype, qclass, response, source, created_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (name, qtype, qclass) DO UPDATE SET \
             response = excluded.response, source = excluded.source, \
             created_at = excluded.created_at, expires_at = excluded.expires_at",
        )
        .bind(cache_name(query))
        .bind(u16::from(query.query_type()))
        .bind(u16::from(query.query_class()))
        .bind(message.to_vec().context("Error encoding answer")?)
        .bind(source.as_str())
        .bind(now)
        .bind(now + i64::from(ttl))
        .execute(&self.pool)
        .await?;

        Ok(true)
    }

    pub async fn purge_expired(&self, now: i64) -> anyhow::Result<u64> {
        Ok(sqlx::query("DELETE FROM dns_cache WHERE expires_at <= ?")
            .bind(now)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }
}

fn cache_name(query: &Query) -> String {
    query.name().to_lowercase().to_string()
}

/// How long to keep an answer, or `None` when it should not be cached.
pub fn cache_ttl(message: &Message, options: &CacheOptions) -> Option<u32> {
    let negative = match message.response_code() {
        ResponseCode::NXDomain => true,
        ResponseCode::NoError => message.answers().is_empty(),
        _ => return None,
    };

    if negative {
        // RFC 2308: negative answers live for the smaller of the SOA TTL and its minimum field.
        let soa_ttl = message
            .name_servers()
            .iter()
            .find_map(|r| match r.data() {
                RData::SOA(soa) => Some(r.ttl().min(soa.minimum())),
                _ => None,
            })
            .unwrap_or(options.negative_ttl);
        return Some(soa_ttl.min(options.negative_ttl)).filter(|ttl| *ttl > 0);
    }

    let ttl = message.answers().iter().map(|r| r.ttl()).min()?;
    Some(ttl.clamp(options.min_ttl, options.max_ttl.max(options.min_ttl))).filter(|ttl| *ttl > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns_split::policy::tests::{answer, query};
    use hickory_proto::rr::rdata::SOA;
    use hickory_proto::rr::{Name, RecordType};
    use std::net::Ipv4Addr;

    const OPTIONS: CacheOptions = CacheOptions {
        min_ttl: 30,
        max_ttl: 3600,
        negative_ttl: 300,
    };

    fn question() -> Query {
        query("Example.COM.", RecordType::A).queries()[0].clone()
    }

    fn with_ttl(mut message: Message, ttl: u32) -> Message {
        for r in message.answers_mut() {
            r.set_ttl(ttl);
        }
        message
    }

    fn negative(code: ResponseCode, soa_ttl: u32, soa_minimum: u32) -> Message {
        let mut message = answer(&[]);
        message.set_response_code(code);
        message.add_name_server(Record::from_rdata(
            Name::from_ascii("com.").unwrap(),
            soa_ttl,
            RData::SOA(SOA::new(
                Name::from_ascii("a.gtld-servers.net.").unwrap(),
                Name::from_ascii("nstld.verisign-grs.com.").unwrap(),
                1,
                1800,
                900,
                604800,
                soa_minimum,
            )),
        ));
        message
    }

    #[test]
    fn ttl_is_clamped() {
        let ip = Ipv4Addr::new(1, 2, 3, 4);
        assert_eq!(cache_ttl(&with_ttl(answer(&[ip]), 5), &OPTIONS), Some(30));
        assert_eq!(
            cache_ttl(&with_ttl(answer(&[ip]), 600), &OPTIONS),
            Some(600)
        );
        assert_eq!(
            cache_ttl(&with_ttl(answer(&[ip]), 86400), &OPTIONS),
            Some(3600)
        );
    }

    #[test]
    fn negative_ttl_uses_soa() {
        assert_eq!(
            cache_ttl(&negative(ResponseCode::NXDomain, 900, 60), &OPTIONS),
            Some(60)
        );
        assert_eq!(
            cache_ttl(&negative(ResponseCode::NoError, 45, 900), &OPTIONS),
            Some(45)
        );
        assert_eq!(
            cache_ttl(&negative(ResponseCode::NXDomain, 9000, 9000), &OPTIONS),
            Some(300)
        );

        let mut no_soa = answer(&[]);
        no_soa.set_response_code(ResponseCode::NXDomain);
        assert_eq!(cache_ttl(&no_soa, &OPTIONS), Some(300));
    }

    #[test]
    fn failures_are_not_cached() {
        let mut servfail = answer(&[]);
        servfail.set_response_code(ResponseCode::ServFail);
        assert_eq!(cache_ttl(&servfail, &OPTIONS), None);
    }

    #[tokio::test]
    async fn round_trip_counts_down_and_expires() {
        let cache = DnsCache::open_in_memory(OPTIONS).await.unwrap();
        let q = question();
        let message = with_ttl(answer(&[Ipv4Addr::new(1, 2, 3, 4)]), 300);

        assert!(cache.get(&q, 1000).await.unwrap().is_none());
        assert!(
            cache
                .put(&q, &message, Group::Alternative, 1000)
                .await
                .unwrap()
        );

        // Lookups ignore the case of the name.
        let lower = query("example.com.", RecordType::A).queries()[0].clone();
        let hit = cache.get(&lower, 1100).await.unwrap().unwrap();
        assert_eq!(hit.answers()[0].ttl(), 200);

        assert!(cache.get(&q, 1300).await.unwrap().is_none());
        assert_eq!(cache.purge_expired(1300).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn put_overwrites_existing_entry() {
        let cache = DnsCache::open_in_memory(OPTIONS).await.unwrap();
        let q = question();
        let first = with_ttl(answer(&[Ipv4Addr::new(1, 1, 1, 1)]), 300);
        let second = with_ttl(answer(&[Ipv4Addr::new(2, 2, 2, 2)]), 300);

        cache.put(&q, &first, Group::Upstream, 1000).await.unwrap();
        cache
            .put(&q, &second, Group::Alternative, 1500)
            .await
            .unwrap();

        let hit = cache.get(&q, 1600).await.unwrap().unwrap();
        assert_eq!(
            hit.answers()[0].data(),
            &RData::A(Ipv4Addr::new(2, 2, 2, 2).into())
        );
    }

    #[tokio::test]
    async fn uncacheable_answers_are_skipped() {
        let cache = DnsCache::open_in_memory(OPTIONS).await.unwrap();
        let mut servfail = answer(&[]);
        servfail.set_response_code(ResponseCode::ServFail);
        assert!(
            !cache
                .put(&question(), &servfail, Group::Upstream, 1000)
                .await
                .unwrap()
        );
        assert!(cache.get(&question(), 1000).await.unwrap().is_none());
    }
}
