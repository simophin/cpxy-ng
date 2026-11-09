use anyhow::Context;
use cpxy_ng::Key;
use cpxy_ng::key_util::derive_password;
use std::fmt::{Debug, Formatter};
use std::str::FromStr;
use url::Url;

#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub key: Key,
    pub tls: bool,
}

impl Debug for Config {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}://{}:{}",
            if self.tls { "https" } else { "http" },
            self.host,
            self.port
        )
    }
}

impl FromStr for Config {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let url: Url = s.parse().context("invalid url")?;
        url.try_into()
    }
}

impl TryFrom<Url> for Config {
    type Error = anyhow::Error;

    fn try_from(value: Url) -> Result<Self, Self::Error> {
        // URL IPv6 literals include brackets, but socket lookup and TLS names take
        // the bare address. Keep upstream transport independent of router-client IPv6.
        let host = match value.host().context("Expected host in URL but got none")? {
            url::Host::Ipv6(address) => address.to_string(),
            host => host.to_string(),
        };
        let port = value
            .port_or_known_default()
            .context("Expected port in URL")?;
        let key = value
            .password()
            .context("Expected password (pre-shared key) in URL")?;
        let key = derive_password(key);
        let tls = match value.scheme() {
            "http" => false,
            "https" => true,
            scheme => anyhow::bail!("Unsupported URL scheme: {scheme}"),
        };

        Ok(Config {
            host,
            port,
            key: key.into(),
            tls,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_hosts_are_ready_for_socket_lookup() {
        for (authority, expected) in [
            ("proxy.example", "proxy.example"),
            ("192.0.2.1", "192.0.2.1"),
            ("[2001:db8::1]", "2001:db8::1"),
        ] {
            let config: Config = format!("http://:test@{authority}:8443").parse().unwrap();
            assert_eq!(config.host, expected);
            assert_eq!(config.port, 8443);
        }
    }
}
