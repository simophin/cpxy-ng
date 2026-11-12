//! HTTP proxy requests. Only `CONNECT` tunnels are supported: forwarding plain `GET
//! http://...` style requests is rejected.
use crate::cipher_select::select_cipher_based_on_port;
use crate::http_stream::HttpStream;
use crate::outbound::{OutboundHost, OutboundRequest};
use crate::protocol;
use anyhow::Context;
use std::fmt::{Display, Formatter};
use tokio::io::AsyncRead;

#[derive(Debug, PartialEq)]
pub struct ProxyRequest {
    pub host: String,
    pub port: u16,
}

/// The client used a method other than `CONNECT`.
#[derive(Debug)]
pub struct MethodNotAllowed(pub String);

impl Display for MethodNotAllowed {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Unsupported HTTP proxy method {}, only CONNECT is allowed",
            self.0
        )
    }
}

impl std::error::Error for MethodNotAllowed {}

impl From<ProxyRequest> for protocol::Request {
    fn from(req: ProxyRequest) -> Self {
        let (client_send_cipher, server_send_cipher) = select_cipher_based_on_port(req.port);
        Self {
            host: req.host,
            port: req.port,
            tls: false,
            server_send_cipher,
            initial_plaintext: vec![],
            client_send_cipher,
            timestamp_epoch_seconds: 0,
        }
    }
}

impl From<ProxyRequest> for OutboundRequest {
    fn from(req: ProxyRequest) -> Self {
        Self {
            host: OutboundHost::Domain(req.host),
            port: req.port,
            initial_plaintext: vec![],
        }
    }
}

pub async fn parse_http_proxy_stream<S: AsyncRead + Unpin>(
    stream: S,
) -> Result<HttpStream<ProxyRequest, S>, (anyhow::Error, S)> {
    HttpStream::parse_request(stream, |req| {
        let method = req.method.context("Expecting http method")?;
        if !method.eq_ignore_ascii_case("CONNECT") {
            return Err(MethodNotAllowed(method.to_string()).into());
        }

        let (host, port_str) = req
            .path
            .context("Expecting CONNECT path")?
            .rsplit_once(':')
            .context("Expecting host:port in CONNECT path")?;

        let port: u16 = port_str.parse().context("Expecting port")?;
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);

        Ok(ProxyRequest {
            host: host.to_string(),
            port,
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parses_connect() {
        let mut req =
            b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com\r\nUser-Agent: Test\r\n\r\n"
                .as_slice();
        let req = parse_http_proxy_stream(&mut req).await.expect("To parse");

        assert_eq!(
            req.head(),
            &ProxyRequest {
                host: "example.com".to_string(),
                port: 443,
            }
        );
    }

    #[tokio::test]
    async fn parses_connect_to_ipv6() {
        let mut req = b"CONNECT [2001:db8::1]:443 HTTP/1.1\r\n\r\n".as_slice();
        let req = parse_http_proxy_stream(&mut req).await.expect("To parse");

        assert_eq!(
            req.head(),
            &ProxyRequest {
                host: "2001:db8::1".to_string(),
                port: 443,
            }
        );
    }

    #[tokio::test]
    async fn rejects_forward_proxy_requests() {
        for req in [
            b"GET http://example.com/path HTTP/1.1\r\nHost: example.com\r\n\r\n".as_slice(),
            b"POST https://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n".as_slice(),
        ] {
            let mut req = req;
            let Err((err, _)) = parse_http_proxy_stream(&mut req).await else {
                panic!("Expected rejection");
            };
            assert!(err.downcast_ref::<MethodNotAllowed>().is_some(), "{err:?}");
        }
    }
}
