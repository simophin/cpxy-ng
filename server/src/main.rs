mod routing;
mod server;

use clap::Parser;
use cpxy_ng::{Key, key_util::derive_password};
use dotenvy::dotenv;
use routing::{Router, Rule};
use std::sync::Arc;
use tokio::net::TcpListener;

#[derive(clap::Parser)]
struct CliOptions {
    /// The pre-shared key for encryption/decryption
    #[clap(long, env)]
    key: String,

    /// The address to listen on for the http proxy
    #[clap(env, default_value = "127.0.0.1:9000")]
    bind_addr: String,

    /// Route requests to a SOCKS5 server based on the HTTP `Host` header the client
    /// connected with, as `<HOST_PATTERN>=<TARGET>`. HOST_PATTERN is `example.com`
    /// (also matches subdomains), `=example.com` (exact) or `*`; TARGET is
    /// `socks5://host:port` or `direct`. The most specific match wins; requests that
    /// match no rule connect directly. Repeatable, or comma-separated in the env var.
    #[clap(long, env = "SOCKS5_ROUTES", value_delimiter = ',')]
    socks5_route: Vec<Rule>,
}

#[tokio::main]
async fn main() {
    let _ = dotenv();
    tracing_subscriber::fmt::init();

    let CliOptions {
        key,
        bind_addr,
        socks5_route,
    } = CliOptions::parse();

    for rule in &socks5_route {
        tracing::info!(?rule, "Server: SOCKS5 route configured");
    }
    let router = Arc::new(Router::new(socks5_route));

    let listener = TcpListener::bind(bind_addr)
        .await
        .expect("Error binding address");

    tracing::info!("Server listening on {}", listener.local_addr().unwrap());

    let key: Key = derive_password(&key).into();

    loop {
        let (socket, addr) = listener.accept().await.expect("Error accepting connection");
        tracing::info!(?addr, "Server: accepted client connection");
        tokio::spawn(server::handle_connection(socket, addr, key, router.clone()));
    }
}
