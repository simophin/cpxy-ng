use crate::handshaker::Handshaker;
use anyhow::Context;
use cpxy_ng::cipher_select::select_cipher_based_on_port;
use cpxy_ng::encrypt_stream::Configuration;
use cpxy_ng::http_proxy::{
    ProxyRequest, ProxyRequestHttp, ProxyRequestSocket, parse_http_proxy_stream,
};
use cpxy_ng::http_stream::HttpStream;
use cpxy_ng::key_util::random_vec;
use cpxy_ng::time_util::now_epoch_seconds;
use cpxy_ng::{http_protocol, protocol};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

pub struct HttpProxyHandshaker<S> {
    stream: HttpStream<(), S>,
    is_tunnel: bool,
}

impl<S> Handshaker<S> for HttpProxyHandshaker<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    type StreamType = HttpStream<(), S>;
    type RequestType = ProxyRequest;

    fn can_read_initial_data(r: &Self::RequestType) -> bool {
        match r {
            ProxyRequest::Http(_) => false,
            ProxyRequest::Socket(_) => true,
        }
    }

    async fn accept(stream: S) -> anyhow::Result<(ProxyRequest, HttpProxyHandshaker<S>)> {
        let (req, stream) = parse_http_proxy_stream(stream)
            .await
            .map_err(|(e, _)| e)?
            .take_head();

        let is_tunnel = matches!(&req, ProxyRequest::Socket(..));
        Ok((req, HttpProxyHandshaker { stream, is_tunnel }))
    }

    async fn respond_ok(mut self) -> anyhow::Result<HttpStream<(), S>> {
        if self.is_tunnel {
            self.stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
        }

        Ok(self.stream)
    }

    async fn respond_err(mut self, msg: &str) -> anyhow::Result<()>
    where
        S: AsyncWrite + Unpin,
    {
        self.stream
            .write_all(construct_error_http_response(500, msg).as_bytes())
            .await?;
        Ok(())
    }

    fn stream_mut(&mut self) -> &mut HttpStream<(), S> {
        &mut self.stream
    }
}

pub async fn http_proxy_request_to_protocol_request(
    request: ProxyRequest,
    conn: &mut (impl AsyncRead + Unpin),
    config: &super::protocol_config::Config,
) -> anyhow::Result<http_protocol::Request> {
    match request {
        ProxyRequest::Http(ProxyRequestHttp {
            host,
            port,
            tls,
            payload,
        }) => {
            let client_send_cipher = Configuration::random_full();
            let server_send_cipher = Configuration::random_full();

            Ok(http_protocol::Request {
                request: protocol::Request {
                    host,
                    port,
                    tls,
                    client_send_cipher,
                    server_send_cipher,
                    initial_plaintext: payload,
                    timestamp_epoch_seconds: now_epoch_seconds(),
                },
                websocket_key: random_vec(16),
                host: config.host.clone(),
            })
        }

        ProxyRequest::Socket(ProxyRequestSocket { host, port }) => {
            let (client_send_cipher, server_send_cipher) = select_cipher_based_on_port(port);
            let mut initial_plaintext = vec![0u8; 256];

            match timeout(
                Duration::from_millis(200),
                conn.read(&mut initial_plaintext),
            )
            .await
            {
                Ok(Ok(n)) => initial_plaintext.truncate(n),
                Err(_) => initial_plaintext.clear(), // Timeout, no initial data
                Ok(Err(e)) => return Err(e).context("Reading initial plaintext from client"),
            }

            Ok(http_protocol::Request {
                request: protocol::Request {
                    host,
                    port,
                    tls: false,
                    client_send_cipher,
                    server_send_cipher,
                    initial_plaintext,
                    timestamp_epoch_seconds: now_epoch_seconds(),
                },
                websocket_key: random_vec(16),
                host: config.host.clone(),
            })
        }
    }
}

pub fn construct_error_http_response(code: u16, msg: &str) -> String {
    format!(
        "HTTP/1.1 {code} Internal Error\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
        msg.as_bytes().len(),
        msg
    )
}
