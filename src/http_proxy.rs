use anyhow::{Context, ensure};
use bytes::Bytes;
use tokio::io::AsyncRead;
use url::Url;

pub struct ProxyRequest {
    host: String,
    port: u16,
    tls: bool,
    payload: Vec<u8>,
}

impl ProxyRequest {
    pub async fn from_http_stream(
        stream: &mut (impl AsyncRead + Unpin),
    ) -> anyhow::Result<(Self, Bytes)> {
        super::http_util::parse_http_request(stream, |req| {
            let url: Url = req
                .path
                .context("Expecting path in HTTP request")?
                .parse()
                .context("Parsing URL from HTTP request path")?;

            let scheme = url.scheme();
            let method = req.method.context("Expecting http method")?;
            let host = url.host_str().context("Expecting host in HTTP request")?;
            let port = url
                .port_or_known_default()
                .context("Port is not specified or unknown")?;

            if method.eq_ignore_ascii_case("CONNECT") {
                ensure!(scheme == "", "CONNECT method should not have a scheme");
                Ok(Self {
                    host: host.to_string(),
                    port,
                })
            } else {
                ensure!(
                    tls: false,
                    payload: Vec::new(),
                    scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"),
                    "Unsupported URL scheme: {scheme}"
                );
            }
        })
        .await
    }
}
