//! `cpxy`: the cpxy server, client and split DNS server in one binary.
//!
//! Every option can also be given in an environment variable, or in a `.env` file in the
//! working directory; `cpxy <command> --help` names them.

mod routes;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Accept tunnels from cpxy clients and connect them to their destinations
    Server(server::Args),
    /// Serve local HTTP, SOCKS5 and transparent proxies through a cpxy server
    Client(client::cli::Args),
    /// Serve split DNS: local-region answers from upstream servers, the rest from alternatives
    #[cfg(feature = "dns")]
    Dns(client::dns_split::cli::Args),
    /// Print the IP ranges the CN split routing sends directly
    #[command(subcommand)]
    Routes(routes::Command),
}

fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    let command = match cli.command {
        // Prints to stdout: no runtime or logging needed.
        Command::Routes(command) => return command.run(),
        command => command,
    };

    tracing_subscriber::fmt::init();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::select! {
                r = run(command) => r,
                r = shutdown_signal() => {
                    tracing::info!("Shutting down");
                    r
                }
            }
        })
}

async fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Server(args) => server::run(args).await,
        Command::Client(args) => client::cli::run(args).await,
        #[cfg(feature = "dns")]
        Command::Dns(args) => client::dns_split::cli::run(args).await,
        Command::Routes(_) => unreachable!("handled before the runtime starts"),
    }
}

/// Ctrl-C, or SIGTERM, which is how containers and init systems stop a process.
async fn shutdown_signal() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate())?;
        tokio::select! {
            r = tokio::signal::ctrl_c() => Ok(r?),
            _ = term.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    Ok(tokio::signal::ctrl_c().await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn client_needs_a_listener() {
        let err = Cli::try_parse_from(["cpxy", "client", "--server", "http://:k@h:1"])
            .err()
            .unwrap();
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);

        Cli::try_parse_from([
            "cpxy",
            "client",
            "--server",
            "http://:k@h:1",
            "--routing",
            "cn_split",
            "--socks5-proxy-listen",
            "127.0.0.1:1080",
        ])
        .unwrap();
    }

    #[test]
    fn server_takes_repeated_and_listed_routes() {
        let Cli {
            command: Command::Server(_),
        } = Cli::try_parse_from([
            "cpxy",
            "server",
            "--key",
            "k",
            "--socks5-route",
            "a.com=direct,b.com=direct",
            "--socks5-route",
            "*.c.com=socks5://h:1",
        ])
        .unwrap()
        else {
            panic!("not the server");
        };
    }
}
