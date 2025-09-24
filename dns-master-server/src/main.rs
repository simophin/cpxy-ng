mod resolver;
mod server;

use clap::Parser;
use cpxy_ng::key_util::derive_password;
use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;

#[derive(Parser)]
struct CliOptions {
    /// The UDP address to bind on
    #[clap(env, default_value = "0.0.0.0:5354")]
    listen: SocketAddr,

    /// The pre-shared key
    #[clap(env, long)]
    key: String,

    /// The DNS servers located in China
    #[clap(long, env, value_delimiter = ',', required = true)]
    cn_dns: Vec<Ipv4Addr>,

    /// The DNS servers located globally
    #[clap(long, env, value_delimiter = ',', required = true)]
    global_dns: Vec<Ipv4Addr>,
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();

    let CliOptions {
        listen,
        key,
        cn_dns,
        global_dns,
    } = CliOptions::parse();

    let key = derive_password(&key);

    let socket = UdpSocket::bind(listen)
        .await
        .expect("Failed to bind UDP port");

    tracing::info!(
        "Using CN DNS servers: {cn_dns:?}, global DNS servers: {global_dns:?}, listening on {}",
        socket.local_addr().unwrap()
    );

    server::serve(socket, key, &cn_dns, &global_dns)
        .await
        .expect("Error running server");
}
