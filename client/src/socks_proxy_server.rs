use crate::handshaker::Handshaker;
use anyhow::Context;
use cpxy_ng::cipher_select::select_cipher_based_on_port;
use cpxy_ng::key_util::random_vec;
use cpxy_ng::socks_stream::{ProxyRequest, SocksStream};
use cpxy_ng::time_util::now_epoch_seconds;
use cpxy_ng::{http_protocol, protocol};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

pub struct SocksProxyHandshaker<S> {
    stream: SocksStream<S>,
}

impl<S> Handshaker<S> for SocksProxyHandshaker<S>
where
    S: AsyncBufRead + AsyncWrite + Unpin,
{
    type StreamType = SocksStream<S>;
    type RequestType = ProxyRequest;

    fn can_read_initial_data(_r: &Self::RequestType) -> bool {
        true
    }

    async fn accept(stream: S) -> anyhow::Result<(ProxyRequest, Self)> {
        let (stream, request) = SocksStream::accept(stream).await.map_err(|(e, _)| e)?;
        Ok((request, SocksProxyHandshaker { stream }))
    }

    async fn respond_ok(mut self) -> anyhow::Result<SocksStream<S>>
    where
        S: AsyncWrite + Unpin,
    {
        self.stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .context("Error writing OK response")?;

        Ok(self.stream)
    }

    async fn respond_err(mut self, msg: &str) -> anyhow::Result<()>
    where
        S: AsyncWrite + Unpin,
    {
        self.stream
            .write_all(super::http_proxy_server::construct_error_http_response(500, msg).as_bytes())
            .await
            .context("Error writing ERR response")
    }

    fn stream_mut(&mut self) -> &mut SocksStream<S> {
        &mut self.stream
    }
}

pub async fn socks_proxy_request_to_protocol_request(
    request: ProxyRequest,
    conn: &mut (impl AsyncRead + Unpin),
    config: &super::protocol_config::Config,
) -> anyhow::Result<http_protocol::Request> {
    let (host, port) = match request {
        ProxyRequest::WithDomain(host, port) => (host, port),
        ProxyRequest::WithIP(addr) => (addr.ip().to_string(), addr.port()),
    };

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
