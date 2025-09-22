use anyhow::Context;
use std::sync::{Arc, LazyLock};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

pub enum TlsClientStream<S> {
    TLS(TlsStream<S>),
    Plain(S),
}

static CONNECTOR: LazyLock<TlsConnector> = LazyLock::new(|| {
    let mut root_cert_store = RootCertStore::empty();
    root_cert_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    TlsConnector::from(Arc::new(
        ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth(),
    ))
});

impl<S: AsyncRead + AsyncWrite + Unpin> TlsClientStream<S> {
    pub async fn connect_tls(domain_name: &str, inner: S) -> anyhow::Result<Self> {
        CONNECTOR
            .connect(
                domain_name
                    .to_string()
                    .try_into()
                    .context("Unable to convert host to server name")?,
                inner,
            )
            .await
            .context("Unable to connect to TLS server")
            .map(TlsClientStream::TLS)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for TlsClientStream<S> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            TlsClientStream::TLS(tls) => std::pin::Pin::new(tls).poll_read(cx, buf),
            TlsClientStream::Plain(plain) => std::pin::Pin::new(plain).poll_read(cx, buf),
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for TlsClientStream<S> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            TlsClientStream::TLS(tls) => std::pin::Pin::new(tls).poll_write(cx, buf),
            TlsClientStream::Plain(plain) => std::pin::Pin::new(plain).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            TlsClientStream::TLS(tls) => std::pin::Pin::new(tls).poll_flush(cx),
            TlsClientStream::Plain(plain) => std::pin::Pin::new(plain).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            TlsClientStream::TLS(tls) => std::pin::Pin::new(tls).poll_shutdown(cx),
            TlsClientStream::Plain(plain) => std::pin::Pin::new(plain).poll_shutdown(cx),
        }
    }
}
