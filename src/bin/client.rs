use chacha20poly1305::Key;
use cpxy_ng::client;
use dotenvy::dotenv;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    let _ = dotenv();
    tracing_subscriber::fmt::init();

    let listener = TcpListener::bind(("127.0.0.1", 8080))
        .await
        .expect("Error binding address");

    tracing::debug!("Started listening on {}", listener.local_addr().unwrap());

    let server = "127.0.0.1".to_string();
    let server_port = 9000;
    let key: Key = Key::from([0u8; 32]);

    loop {
        let (client, addr) = listener.accept().await.expect("Error accepting connection");
        tracing::info!("Accepted connection from {addr}");

        tokio::spawn(client::accept_proxy_connection(
            client,
            server.clone(),
            server_port,
            key,
        ));
    }
}
