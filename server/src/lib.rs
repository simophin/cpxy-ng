//! The cpxy server: accepts tunnels from cpxy clients and connects them to their destinations.

mod routing;
mod server;
mod socks5;

use anyhow::Context;
use cpxy_ng::{Key, key_util::derive_password};
use routing::{Router, RuleList};
use std::sync::Arc;
use tokio::net::TcpListener;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The pre-shared key for encryption/decryption
    #[arg(long, env = "KEY", hide_env_values = true)]
    key: String,

    /// The address to listen on for cpxy clients
    #[arg(long, env = "BIND_ADDR", default_value = "127.0.0.1:9000")]
    listen: String,

    /// Route requests to a SOCKS5 server based on the HTTP `Host` header the client
    /// connected with, as `<HOST_PATTERN>=<TARGET>`. HOST_PATTERN must match the whole
    /// host (case-insensitive, port stripped) and may contain any number of `*`
    /// wildcards, each matching any run of characters including dots, e.g.
    /// `*.example.com` or `us*.proxy.*.net`. TARGET is `socks5://host:port` or `direct`.
    /// Rules are tried in order and the first match wins; requests that match no rule
    /// connect directly. Repeatable, and each value may hold several rules separated by
    /// commas and/or whitespace.
    #[arg(long, env = "SOCKS5_ROUTES")]
    socks5_route: Vec<RuleList>,
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    let Args {
        key,
        listen,
        socks5_route,
    } = args;

    let rules: Vec<_> = socks5_route.into_iter().flat_map(|list| list.0).collect();
    for rule in &rules {
        tracing::info!(%rule, "Server: SOCKS5 route configured");
    }
    let router = Arc::new(Router::new(rules));
    router.warm_up();

    let listener = TcpListener::bind(&listen)
        .await
        .with_context(|| format!("Error binding {listen}"))?;
    tracing::info!("Server listening on {}", listener.local_addr()?);

    let key: Key = derive_password(&key).into();

    loop {
        let (socket, addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            // Such as running out of file descriptors: back off instead of exiting.
            Err(e) => {
                tracing::error!(?e, "Error accepting connection");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        tracing::info!(?addr, "Server: accepted client connection");
        tokio::spawn(server::handle_connection(socket, addr, key, router.clone()));
    }
}
