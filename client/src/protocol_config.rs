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
        f.debug_struct("Config")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("key", &"<redacted>")
            .field("tls", &self.tls)
            .finish()
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
        let host = value
            .host_str()
            .context("Expected host in URL but got none")?
            .to_string();
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
