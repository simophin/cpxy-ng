use anyhow::{Context, ensure};
use cpxy_ng::geoip::find_country_code_v4;
use geoip_data::CN_GEOIP;
use std::fmt::Debug;
use std::net::IpAddr;
use tokio::net::lookup_host;
use tracing::instrument;

#[derive(Debug, Copy, Clone)]
pub enum Action {
    Direct(IpAddr),
    Proxy,
}

#[instrument(ret)]
pub async fn divert_action(domain: impl AsRef<str> + Debug) -> anyhow::Result<Action> {
    let addrs: Vec<_> = lookup_host(domain.as_ref())
        .await
        .context("Error looking up host")?
        .collect();

    ensure!(
        !addrs.is_empty(),
        "No addresses found for domain {domain:?}"
    );

    match addrs.iter().position(|a| match a.ip() {
        IpAddr::V4(ipv4) => matches!(find_country_code_v4(&ipv4, CN_GEOIP), Ok(Some("CN"))),
        IpAddr::V6(_) => false,
    }) {
        Some(idx) => Ok(Action::Direct(addrs[idx].ip())),
        None => Ok(Action::Proxy),
    }
}
