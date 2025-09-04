mod server;

use clap::Parser;
use cpxy_ng::{key_util::derive_password, Key};
use dotenvy::dotenv;
use tokio::net::TcpListener;

#[derive(clap::Parser)]
struct CliOptions {
    /// The pre-shared key for encryption/decryption
    #[clap(long, env)]
    key: String,

    /// The address to listen on for the http proxy
    #[clap(env, default_value = "127.0.0.1:8080")]
    bind_addr: String,
}

#[tokio::main]
async fn main() {
    let _ = dotenv();
    tracing_subscriber::fmt::init();

    let CliOptions { key, bind_addr } = CliOptions::parse();

    let listener = TcpListener::bind(bind_addr)
        .await
        .expect("Error binding address");

    tracing::info!("Server listening on {}", listener.local_addr().unwrap());

    let key: Key = derive_password(&key).into();
    let connector = server::configure_tls_connector();

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
