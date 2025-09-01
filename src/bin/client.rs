use anyhow::Context;
use chacha20poly1305::aead::OsRng;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit};
use cpxy_ng::http_proxy;
use dotenvy::dotenv;
use hyper::body::Body;
use tokio::net::{TcpListener, TcpStream};

#[tokio::main]
async fn main() {
    dotenv().unwrap();
    tracing_subscriber::fmt::init();

    let listener = TcpListener::bind(("127.0.0.1", 8080))
        .await
        .expect("Error binding address");

    tracing::debug!("Started listening on {}", listener.local_addr().unwrap());

    let server = "127.0.0.1".to_string();
    let server_port = 9000;
    let key = ChaCha20Poly1305::generate_key(&mut OsRng);

    loop {
        let (client, addr) = listener.accept().await.expect("Error accepting connection");
        tracing::info!("Accepted connection from {addr}");

        let server = server.clone();
        let key = key.clone();
        let server_port = server_port;

        tokio::spawn(async move {
            if let Err(e) = accept_connection(client, server, server_port, key).await {
                tracing::error!("Error handling connection from {addr}: {e:?}");
            }
        });
    }
}

async fn accept_connection(
    mut s: TcpStream,
    server_host: String,
    server_port: u16,
    key: Key,
) -> anyhow::Result<()> {
    let (req, extra_data) = http_proxy::ProxyRequest::from_http_stream(&mut s)
        .await
        .context("Error parsing HTTP proxy request")?;

    tracing::debug!(?req, "Successfully parsed request");

    todo!()
}
