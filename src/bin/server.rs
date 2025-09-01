use chacha20poly1305::Key;
use cpxy_ng::server;
use cpxy_ng::server::configure_tls_connector;
use dotenvy::dotenv;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

#[tokio::main]
async fn main() {
    let _ = dotenv();
    tracing_subscriber::fmt::init();

    let listener = TcpListener::bind(("127.0.0.1", 9000))
        .await
        .expect("Error binding address");

    tracing::info!("Server listening on {}", listener.local_addr().unwrap());

    let key: Key = Key::from([0u8; 32]);
    let connector = configure_tls_connector();

    loop {
        let (socket, addr) = listener.accept().await.expect("Error accepting connection");
        tokio::spawn(server::handle_connection(
            socket,
            addr,
            key,
            connector.clone(),
        ));
    }
}
