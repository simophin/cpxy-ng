use cpxy_ng::dns_model::{DnsResolveResult, SingleResolveResult};
use cpxy_ng::geoip::find_country_code_v4;
use futures::future::join_all;
use geoip_data::CN_GEOIP;
use hickory_resolver::TokioResolver;
use std::fmt::Debug;
use std::time::Instant;
use tokio::join;
use tracing::instrument;

#[derive(Default, Debug)]
struct ResolveResult<'a> {
    domain: &'a str,
    result: SingleResolveResult,
    contains_cn_address: bool,
}

#[instrument(ret, skip(resolver))]
async fn resolve_single<'a>(domain: &'a str, resolver: &TokioResolver) -> ResolveResult<'a> {
    let result = match resolver.ipv4_lookup(domain).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(?e, "Error looking up");
            return Default::default();
        }
    };

    let addresses: Vec<_> = result.iter().map(|ip| ip.0).collect();

    ResolveResult {
        domain,
        contains_cn_address: addresses
            .iter()
            .any(|a| matches!(find_country_code_v4(a, CN_GEOIP), Ok(Some("CN")))),
        result: SingleResolveResult {
            ttl: result.valid_until().duration_since(Instant::now()),
            addresses,
        },
    }
}

#[instrument(ret, skip(cn_resolver, global_resolver))]
pub async fn resolve_dns_request(
    domains: &[impl AsRef<str> + Debug],
    cn_resolver: &TokioResolver,
    global_resolver: &TokioResolver,
) -> anyhow::Result<DnsResolveResult> {
    let cn_results = join_all(
        domains
            .iter()
            .map(|domain| resolve_single(domain.as_ref(), cn_resolver)),
    );

    let global_results = join_all(
        domains
            .iter()
            .map(|domain| resolve_single(domain.as_ref(), global_resolver)),
    );

    let (cn_results, global_results) = join!(cn_results, global_results);

    Ok(DnsResolveResult {
        result: cn_results
            .into_iter()
            .zip(global_results.into_iter())
            .map(|(cn_result, global_result)| {
                (
                    cn_result.domain.to_string(),
                    if cn_result.contains_cn_address || global_result.result.addresses.is_empty() {
                        cn_result.result
                    } else {
                        global_result.result
                    },
                )
            })
            .collect(),
    })
}
