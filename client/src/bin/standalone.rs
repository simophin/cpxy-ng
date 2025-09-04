use clap::Parser;
use cpxy_ng::key_util::derive_password;
use dotenvy::dotenv;
use tokio::net::TcpListener;

#[derive(clap::Parser)]
struct CliOptions {
    /// The cpxy sever host to connect to
    #[clap(long, env)]
    server_host: String,

    /// The cpxy server port to connect to
    #[clap(long, env)]
    server_port: u16,

    /// The pre-shared key for encryption/decryption
    #[clap(long, env)]
    key: String,

    /// Whether to use websocket to connect to the server
    #[clap(long, env, default_value_t = false)]
    use_websocket: bool,

    /// The address to listen on for the http proxy
    #[clap(env, default_value = "127.0.0.1:8080")]
    bind_addr: String,
}

#[tokio::main]
async fn main() {
    let _ = dotenv();
    tracing_subscriber::fmt::init();

    let CliOptions {
        server_host,
        server_port,
        key,
        bind_addr,
        use_websocket,
    } = CliOptions::parse();

    let listener = TcpListener::bind(bind_addr)
        .await
        .expect("Error binding address");

    tracing::info!(
        "Proxy server listening on {}",
        listener.local_addr().unwrap()
    );

    let key = derive_password(&key).into();

    loop {
        let (client, addr) = listener.accept().await.expect("Error accepting connection");
        tracing::info!("Accepted connection from {addr}");

        tokio::spawn(client::client::accept_proxy_connection(
            client,
            server_host.clone(),
            server_port,
            key,
            use_websocket,
        ));
    }
}
