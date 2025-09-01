use crate::http_util::HttpHeaderExt;
use crate::protocol;
use anyhow::{Context, ensure};
use base64::Engine;
use base64::prelude::{BASE64_STANDARD, BASE64_URL_SAFE, BASE64_URL_SAFE_NO_PAD};
use bytes::Bytes;
use chacha20poly1305::Key;
use sha1::Digest;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

#[derive(Debug, PartialEq)]
pub struct Request {
    pub request: protocol::Request,
    pub websocket_key: Option<[u8; 16]>,
    pub host: String,
}

impl Request {
    pub async fn from_http_stream<S: AsyncRead + Unpin>(
        stream: &mut S,
        encrypt_key: &Key,
    ) -> anyhow::Result<(Self, Bytes)> {
        super::http_util::parse_http_request(stream, |http_req| {
            let request = protocol::Request::deserialize_from_url_path_segments(
                http_req.path.context("Expected a URL path but got none")?,
                encrypt_key,
            )
            .context("Deserializing request from URL path")?;

            let use_websocket = http_req
                .headers
                .get_header_value("Upgrade")
                .map(|v| v.eq_ignore_ascii_case(b"websocket"))
                .unwrap_or(false);

            let websocket_key = if use_websocket {
                let key_b64 = http_req
                    .headers
                    .get_header_value("Sec-WebSocket-Key")
                    .and_then(|value| std::str::from_utf8(value).ok())
                    .context("Expected Sec-WebSocket-Key header for websocket request")?;
                let key_bytes = BASE64_URL_SAFE
                    .decode(key_b64)
                    .context("Base64 decoding Sec-WebSocket-Key failed")?;
                ensure!(
                    key_bytes.len() == 16,
                    "Sec-WebSocket-Key must decode to 16 bytes"
                );
                let mut key_array = [0u8; 16];
                key_array.copy_from_slice(&key_bytes);
                Some(key_array)
            } else {
                None
            };

            let host = http_req
                .headers
                .get_header_value("Host")
                .and_then(|value| std::str::from_utf8(value).ok())
                .unwrap_or_default()
                .to_string();

            Ok(Self {
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
            .serialize_as_url_path_segments(encrypt_key)
            .context("Serializing request as URL path segments")?;

        let mut http_request = format!("GET /{request} HTTP/1.1\r\nHost: {}\r\n", self.host);
        if let Some(key) = self.websocket_key {
            let ws_key_b64 = BASE64_URL_SAFE.encode(&key);
            http_request.push_str("Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: ");
            http_request.push_str(ws_key_b64.as_str());
            http_request.push_str("\r\n\r\n");
        } else {
            http_request.push_str("Connection: close\r\n\r\n");
        }

        stream
            .write_all(http_request.as_bytes())
            .await
            .context("Writing HTTP request to stream")?;
        Ok(())
    }
}

pub struct Response {
    pub response: protocol::Response,
    pub websocket_key: Option<[u8; 16]>,
}

impl Response {
    pub async fn from_http_stream<S: AsyncRead + Unpin>(
        stream: &mut S,
        encrypt_key: &Key,
    ) -> anyhow::Result<(Self, Bytes)> {
        super::http_util::parse_http_response(stream, |http_res| {
            let bytes = http_res
                .headers
                .get_header_value(PROTOCOL_RESPONSE_HEADER)
                .context("Unable to find response header")?;

            let bytes = BASE64_URL_SAFE_NO_PAD
                .decode(bytes)
                .context("Base64 decoding response header failed")?;

            let response = protocol::Response::deserialize(&bytes, encrypt_key)
                .context("Error deserialize response")?;

            Ok(Self {
                response,
                websocket_key: None,
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

        let response = match &self.websocket_key {
            Some(key) => {
                let mut hasher = sha1::Sha1::new();
                hasher.update(key);
                hasher.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
                let accept_key = hasher.finalize();
                let accept_key_b64 = BASE64_STANDARD.encode(&accept_key);

                format!(
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept_key_b64}\r\n{PROTOCOL_RESPONSE_HEADER}: {response}\r\n\r\n",
                )
            }
            None => {
                format!("HTTP/1.1 200 OK\r\n{PROTOCOL_RESPONSE_HEADER}: {response}\r\n\r\n",)
            }
        };

        stream
            .write_all(response.as_bytes())
            .await
            .context("Writing HTTP response to stream")?;
        Ok(())
    }
}

const PROTOCOL_RESPONSE_HEADER: &str = "ETag";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encrypt_stream::Configuration;
    use chacha20poly1305::aead::OsRng;
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
    use rand::random;
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
            websocket_key: Some(random()),
            host: "example.com".to_string(),
        };

        let (_, received_request) = try_join!(
            request.send_over_http(&mut client, &encrypt_key),
            Request::from_http_stream(&mut server, &encrypt_key)
        )
        .expect("To send/receive request");

        assert_eq!(request, received_request.0);

        let response = Response {
            response: protocol::Response::Success {
                initial_response: vec![6, 7, 8, 9, 10],
                timestamp_epoch_seconds: 54321,
            },
            websocket_key: request.websocket_key,
        };

        let (_, received_response) = try_join!(
            response.send_over_http(&mut client, &encrypt_key),
            Response::from_http_stream(&mut server, &encrypt_key)
        )
        .expect("To send/receive response");

        assert_eq!(response.response, received_response.0.response);
    }
}
