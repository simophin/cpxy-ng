use crate::either_stream::EitherStream;
use anyhow::Context;
use std::sync::{Arc, LazyLock};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

static CONNECTOR: LazyLock<TlsConnector> = LazyLock::new(|| {
    let mut root_cert_store = RootCertStore::empty();
    root_cert_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    TlsConnector::from(Arc::new(
        ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth(),
    ))
});

pub async fn connect_tls<S: AsyncRead + AsyncWrite + Unpin>(
    domain_name: &str,
    tls: bool,
    inner: S,
) -> anyhow::Result<EitherStream<TlsStream<S>, S>> {
    if tls {
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
            .map(EitherStream::Left)
    } else {
        Ok(EitherStream::Right(inner))
    }
}
