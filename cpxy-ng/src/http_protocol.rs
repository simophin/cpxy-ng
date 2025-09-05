use crate::http_stream::HttpStream;
use crate::http_util::HttpHeaderExt;
use crate::protocol;
use anyhow::{Context, ensure};
use base64::Engine;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use chacha20poly1305::Key;
use rand::random_range;
use sha1::Digest;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

#[derive(Debug, PartialEq)]
pub struct Request {
    pub request: protocol::Request,
    pub websocket_key: Vec<u8>,
    pub host: String,
}

static ALL_METHODS: &[&str] = &["GET", "POST", "PATCH", "PUT"];

static USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:141.0) Gecko/20100101 Firefox/141.0",
];

const REQUEST_OVERFLOW_HEADER: &str = "Authorization";

impl Request {
    pub async fn parse<S: AsyncRead + Unpin>(
        stream: S,
        encrypt_key: &Key,
    ) -> Result<HttpStream<Request, S>, (anyhow::Error, S)> {
        HttpStream::parse_request(stream, |http_req| {
            let serialized = format!(
                "{}{}",
                http_req.path.context("Expected a URL path but got none")?,
                http_req
                    .headers
                    .get_header_value_str(REQUEST_OVERFLOW_HEADER)
                    .unwrap_or_default()
            );

            let request = protocol::Request::deserialize(&serialized, encrypt_key)
                .context("Deserializing request from URL path")?;

            ensure!(
                matches!(http_req.headers.get_header_value("upgrade"),
                    Some(v) if v.eq_ignore_ascii_case(b"websocket")),
                "No upgrade header found"
            );

            let websocket_key = http_req
                .headers
                .get_header_value("Sec-WebSocket-Key")
                .and_then(|value| BASE64_URL_SAFE_NO_PAD.decode(value).ok())
                .context("Expected Sec-WebSocket-Key header for websocket request")?;

            let host = http_req
                .headers
                .get_header_value("Host")
                .and_then(|value| std::str::from_utf8(value).ok())
                .unwrap_or_default()
                .to_string();

            Ok(Request {
                request,
                websocket_key,
                host,
            })
        })
        .await
    }

    pub async fn send_over_http(
        &self,
        stream: &mut (impl AsyncWrite + Unpin),
        encrypt_key: &Key,
    ) -> anyhow::Result<()> {
        let request = self
            .request
            .serialize(encrypt_key)
            .context("Serializing request as URL path segments")?;

        let method = ALL_METHODS[random_range(0..ALL_METHODS.len())];

        let (path, overflow) = request.split_at(request.len().min(25));

        let mut http_request = vec![0u8; 0];
        use std::io::Write;
        let _ = write!(&mut http_request, "{method} /{path} HTTP/1.1\r\n");
        let _ = write!(&mut http_request, "Host: {}\r\n", self.host);
        let _ = write!(&mut http_request, "Upgrade: websocket\r\n");
        let _ = write!(&mut http_request, "Connection: Upgrade\r\n");
        let _ = write!(&mut http_request, "Sec-WebSocket-Version: 14\r\n");
        let _ = write!(
            &mut http_request,
            "Sec-WebSocket-Key: {}\r\n",
            BASE64_URL_SAFE_NO_PAD.encode(&self.websocket_key)
        );
        if overflow.len() > 0 {
            let _ = write!(
                &mut http_request,
                "{REQUEST_OVERFLOW_HEADER}: {overflow}\r\n"
            );
        }
        let _ = write!(
            &mut http_request,
            "User-Agent: {}\r\n",
            USER_AGENTS[random_range(0..USER_AGENTS.len())]
        );
        http_request.extend_from_slice(b"\r\n");

        stream
            .write_all(&http_request)
            .await
            .context("Writing HTTP request to stream")?;
        Ok(())
    }
}

pub struct Response {
    pub response: protocol::Response,
    pub websocket_key: Vec<u8>,
}

impl Response {
    pub async fn parse<S: AsyncRead + Unpin>(
        stream: S,
        encrypt_key: &Key,
    ) -> Result<HttpStream<Response, S>, (anyhow::Error, S)> {
        HttpStream::parse_response(stream, |http_res| {
            let bytes = http_res
                .headers
                .get_header_value(PROTOCOL_RESPONSE_HEADER)
                .context("Unable to find response header")?;

            let bytes = BASE64_URL_SAFE_NO_PAD
                .decode(bytes)
                .context("Base64 decoding response header failed")?;

            let websocket_key = http_res
                .headers
                .get_header_value("Sec-WebSocket-Accept")
                .context("Missing Sec-WebSocket-Accept header")
                .and_then(|value| {
                    BASE64_URL_SAFE_NO_PAD
                        .decode(value)
                        .context("Error decode key as b64")
                })?;

            let response = protocol::Response::deserialize(&bytes, encrypt_key)
                .context("Error deserialize response")?;

            Ok(Response {
                response,
                websocket_key,
            })
        })
        .await
    }

    pub async fn send_over_http(
        &self,
        stream: &mut (impl AsyncWrite + Unpin),
        encrypt_key: &Key,
    ) -> anyhow::Result<()> {
        let response = self
            .response
            .serialize(encrypt_key)
            .context("Serializing response")?;
        let response = BASE64_URL_SAFE_NO_PAD.encode(&response);

        let mut hasher = sha1::Sha1::new();
        hasher.update(&self.websocket_key);
        hasher.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        let accept_key = hasher.finalize();
        let accept_key_b64 = BASE64_URL_SAFE_NO_PAD.encode(&accept_key);

        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept_key_b64}\r\n{PROTOCOL_RESPONSE_HEADER}: {response}\r\n\r\n",
        );

        stream
            .write_all(response.as_bytes())
            .await
            .context("Writing HTTP response to stream")?;
        Ok(())
    }
}

const PROTOCOL_RESPONSE_HEADER: &str = "X-Cache-Result";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encrypt_stream::Configuration;
    use chacha20poly1305::aead::OsRng;
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
    use std::num::NonZeroUsize;
    use tokio::try_join;

    #[tokio::test]
    async fn http_protocol_works() {
        let (mut client, mut server) = tokio::io::duplex(32);
        let encrypt_key = ChaCha20Poly1305::generate_key(&mut OsRng);

        let request = Request {
            request: protocol::Request {
                host: "google.com".to_string(),
                port: 23,
                tls: true,
                client_send_cipher: Configuration::random_partial(NonZeroUsize::new(32).unwrap()),
                server_send_cipher: Configuration::random_full(),
                initial_plaintext: vec![1, 2, 3, 4, 5],
                timestamp_epoch_seconds: 12345,
            },
            websocket_key: vec![0u8; 12],
            host: "example.com".to_string(),
        };

        let do_parse_request = async {
            Request::parse(&mut server, &encrypt_key)
                .await
                .map_err(|e| e.0)
        };

        let (_, received_request) = try_join!(
            request.send_over_http(&mut client, &encrypt_key),
            do_parse_request
        )
        .expect("To send/receive request");

        assert_eq!(&request, received_request.head());

        let response = Response {
            response: protocol::Response::Success {
                initial_response: vec![6, 7, 8, 9, 10],
                timestamp_epoch_seconds: 54321,
            },
            websocket_key: request.websocket_key,
        };

        let do_parse_response = async {
            Response::parse(&mut server, &encrypt_key)
                .await
                .map_err(|e| e.0)
        };

        let (_, received_response) = try_join!(
            response.send_over_http(&mut client, &encrypt_key),
            do_parse_response,
        )
        .expect("To send/receive response");

        assert_eq!(response.response, received_response.head().response);
    }
}
