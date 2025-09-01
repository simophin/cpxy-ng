use anyhow::{Context, ensure};
use bytes::Bytes;
use tokio::io::AsyncRead;
use url::Url;

#[derive(Debug, PartialEq)]
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
                    payload: Default::default(),
                    tls: false,
                })
            } else {
                let tls = scheme.eq_ignore_ascii_case("https");
                ensure!(
                    scheme.eq_ignore_ascii_case("http") || tls,
                    "Unsupported URL scheme: {scheme}"
                );
                let version = req.version.context("Expecting HTTP version")?;

                let mut payload = vec![];

                // Status line
                payload.extend_from_slice(method.as_bytes());
                payload.push(b' ');
                payload.extend_from_slice(url.path().as_bytes());
                if let Some(query) = url.query() {
                    payload.push(b'?');
                    payload.extend_from_slice(query.as_bytes());
                }
                match version {
                    1 => payload.extend_from_slice(b" HTTP/1.1\r\n"),
                    0 => payload.extend_from_slice(b" HTTP/1.0\r\n"),
                    2 => payload.extend_from_slice(b" HTTP/2.0\r\n"),
                    _ => anyhow::bail!("Unsupported HTTP version: {version}"),
                }

                // Headers
                for hdr in req.headers.iter() {
                    payload.extend_from_slice(hdr.name.as_bytes());
                    payload.extend_from_slice(b": ");
                    payload.extend_from_slice(hdr.value);
                    payload.extend_from_slice(b"\r\n");
                }

                payload.extend_from_slice(b"\r\n");
                Ok(Self {
                    host: host.to_string(),
                    port,
                    payload,
                    tls,
                })
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn proxy_request_parsing_works() {
        let mut req = b"GET http://example.com/path?query=1 HTTP/1.1\r\nHost: example.com\r\nUser-Agent: Test\r\n\r\n".as_slice();
        let req = ProxyRequest::from_http_stream(&mut req)
            .await
            .expect("To parse")
            .0;

        assert_eq!(req.host, "example.com");
        assert_eq!(req.port, 80);
        assert!(!req.tls);
        assert_eq!(
            req.payload,
            b"GET /path?query=1 HTTP/1.1\r\nHost: example.com\r\nUser-Agent: Test\r\n\r\n"
        );
    }

    #[tokio::test]
    async fn proxy_request_parsing_works_tls() {
        let mut req = b"GET https://example.com/path?query=1 HTTP/1.1\r\nHost: example.com\r\nUser-Agent: Test\r\n\r\n".as_slice();
        let req = ProxyRequest::from_http_stream(&mut req)
            .await
            .expect("To parse")
            .0;

        assert_eq!(req.host, "example.com");
        assert_eq!(req.port, 443);
        assert!(req.tls);
        assert_eq!(
            req.payload,
            b"GET /path?query=1 HTTP/1.1\r\nHost: example.com\r\nUser-Agent: Test\r\n\r\n"
        );
    }
}
