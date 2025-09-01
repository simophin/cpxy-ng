use anyhow::{Context, ensure};
use tokio::io::AsyncRead;
use url::Url;

#[derive(Debug, PartialEq)]
pub struct ProxyRequestHttp {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub payload: Vec<u8>,
}

#[derive(Debug, PartialEq)]
pub struct ProxyRequestSocket {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, PartialEq)]
pub enum ProxyRequest {
    Http(ProxyRequestHttp),
    Socket(ProxyRequestSocket),
}

impl ProxyRequest {
    pub async fn from_http_stream(stream: &mut (impl AsyncRead + Unpin)) -> anyhow::Result<Self> {
        Ok(super::http_util::parse_http_request(stream, |req| {
            let method = req.method.context("Expecting http method")?;

            if method.eq_ignore_ascii_case("CONNECT") {
                let (host, port_str) = req
                    .path
                    .context("Expecting CONNECT path")?
                    .split_once(':')
                    .context("Expecting host:port in CONNECT path")?;

                let port: u16 = port_str.parse().context("Expecting port")?;

                Ok(Self::Socket(ProxyRequestSocket {
                    host: host.to_string(),
                    port,
                }))
            } else {
                let url: Url = req
                    .path
                    .context("Expecting path in HTTP request")?
                    .parse()
                    .context("Parsing URL from HTTP request path")?;

                let scheme = url.scheme();

                let host = url.host_str().context("Expecting host in HTTP request")?;
                let port = url
                    .port_or_known_default()
                    .context("Port is not specified or unknown")?;

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
                Ok(Self::Http(ProxyRequestHttp {
                    host: host.to_string(),
                    port,
                    tls,
                    payload,
                }))
            }
        })
        .await?
        .0)
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
            .expect("To parse");

        assert_eq!(
            req,
            ProxyRequest::Http(ProxyRequestHttp {
                host: "example.com".to_string(),
                port: 80,
                tls: false,
                payload:
                    b"GET /path?query=1 HTTP/1.1\r\nHost: example.com\r\nUser-Agent: Test\r\n\r\n"
                        .to_vec()
            })
        );
    }

    #[tokio::test]
    async fn proxy_request_parsing_works_tls() {
        let mut req = b"GET https://example.com/path?query=1 HTTP/1.1\r\nHost: example.com\r\nUser-Agent: Test\r\n\r\n".as_slice();
        let req = ProxyRequest::from_http_stream(&mut req)
            .await
            .expect("To parse");

        assert_eq!(
            req,
            ProxyRequest::Http(ProxyRequestHttp {
                host: "example.com".to_string(),
                port: 443,
                tls: true,
                payload:
                    b"GET /path?query=1 HTTP/1.1\r\nHost: example.com\r\nUser-Agent: Test\r\n\r\n"
                        .to_vec()
            })
        );
    }

    #[tokio::test]
    async fn proxy_request_parsing_works_socks() {
        let mut req =
            b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com\r\nUser-Agent: Test\r\n\r\n"
                .as_slice();
        let req = ProxyRequest::from_http_stream(&mut req)
            .await
            .expect("To parse");

        assert_eq!(
            req,
            ProxyRequest::Socket(ProxyRequestSocket {
                host: "example.com".to_string(),
                port: 443,
            })
        );
    }
}
