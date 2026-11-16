//! `cpxy reverse-proxy`: a reverse exit, for a machine the cpxy server can't connect to.
//!
//! The exit keeps a few links parked at the server, each a connection it opened with
//! [`ClientMessage::Register`]. When a request the server routes to `reverse://<name>` arrives,
//! the server takes a link and sends a [`LinkCommand::Dial`]; the exit connects to the target
//! from here, answers with a [`DialReply`], and relays. A used link is replaced straight away,
//! and an idle one every [`REVERSE_LINK_MAX_IDLE`], so the server always has fresh ones.

use crate::protocol_config::Config;
use anyhow::{Context, bail, ensure};
use cpxy_ng::encrypt_stream::{CipherStream, Configuration};
use cpxy_ng::http_protocol;
use cpxy_ng::key_util::random_vec;
use cpxy_ng::protocol::{
    ClientEnvelope, ClientMessage, Connected, Dial, DialReply, Failed, Format, LinkCommand,
    REVERSE_LINK_MAX_IDLE, Register, ServerEnvelope, ServerMessage, read_frame, write_frame,
};
use cpxy_ng::target_policy::TargetPolicy;
use cpxy_ng::ws_stream::new_ws_stream;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait before trying again after a link failed to register.
const RETRY_DELAY: Duration = Duration::from_secs(5);
/// Links are replaced up to this much before [`REVERSE_LINK_MAX_IDLE`], so they don't all expire
/// together.
const IDLE_JITTER: Duration = Duration::from_secs(5);

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The cpxy server URL with its key, `http://:<key>@<host>:<port>`
    #[arg(long, env = "SERVER", hide_env_values = true)]
    server: Config,

    /// The name to register as: the server sends requests here that its `reverse://<name>`
    /// routes match
    #[arg(long, env = "REVERSE_NAME")]
    name: String,

    /// How many idle links to keep parked at the server. Each request takes one, and the exit
    /// replaces it a round trip later, so this many requests can start at once without waiting.
    #[arg(long, env = "REVERSE_LINKS", default_value_t = 4)]
    links: usize,

    /// Internal addresses the exit may connect to anyway, as CIDRs or addresses. Otherwise it
    /// refuses targets that resolve only to loopback, private, link-local, CGNAT (including
    /// Tailscale) or other special-purpose addresses, so the server's key doesn't open this
    /// machine's own services and network. Repeatable or comma-separated.
    #[arg(long, env = "ALLOW_TARGETS", value_delimiter = ',', value_parser = TargetPolicy::parse_allowed)]
    allow_target: Vec<ipnet::IpNet>,
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    let Args {
        server,
        name,
        links,
        allow_target,
    } = args;
    ensure!(links > 0, "--links must be at least 1");
    tracing::info!(?server, name, links, "Reverse exit: starting");

    let exit = Arc::new(ReverseExit::new(
        server,
        name,
        TargetPolicy::new(allow_target),
    ));
    futures::future::join_all((0..links).map(|_| exit.clone().keep_link())).await;
    Ok(())
}

/// Parks links at a cpxy server and connects the requests sent down them.
pub struct ReverseExit {
    server: Config,
    name: String,
    targets: TargetPolicy,
}

impl ReverseExit {
    pub fn new(server: Config, name: String, targets: TargetPolicy) -> Self {
        Self {
            server,
            name,
            targets,
        }
    }

    /// Keeps one link parked at the server, forever.
    pub async fn keep_link(self: Arc<Self>) {
        loop {
            if let Err(e) = self.offer_link().await {
                tracing::warn!(error = %format!("{e:#}"), "Reverse exit: link failed");
                sleep(RETRY_DELAY).await;
            }
        }
    }

    /// Registers one link and waits until the server uses it, which hands it to a task that
    /// serves the request, or until it has been idle long enough to replace.
    pub async fn offer_link(&self) -> anyhow::Result<()> {
        let Config { host, port, key } = &self.server;
        let mut conn = timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), *port)))
            .await
            .context("Timed out connecting to the server")?
            .with_context(|| format!("Error connecting to the server {host}:{port}"))?;
        conn.set_nodelay(true)?;

        // A link may carry any kind of traffic, so encrypt all of it.
        let client_send_cipher = Configuration::random_full();
        let server_send_cipher = Configuration::random_full();
        http_protocol::Request {
            message: ClientEnvelope::Current(ClientMessage::Register(Box::new(Register {
                name: self.name.clone(),
                client_send_cipher: client_send_cipher.clone(),
                server_send_cipher: server_send_cipher.clone(),
            }))),
            websocket_key: random_vec(12),
            host: host.clone(),
        }
        .send_over_http(&mut conn, key)
        .await
        .context("Error registering the link")?;

        let (response, conn) = http_protocol::Response::parse(conn, key, Format::Current)
            .await
            .map_err(|(e, _)| e)
            .context("Error reading the server's answer; does it support reverse exits?")?
            .take_head();
        match response.response {
            ServerEnvelope::Current(ServerMessage::Accepted(_)) => {}
            ServerEnvelope::Current(ServerMessage::Rejected(rejected)) => {
                bail!("The server refused the link: {}", rejected.msg)
            }
            ServerEnvelope::Legacy(_) => unreachable!("parsed as {:?}", Format::Current),
        }
        tracing::debug!(name = self.name, "Reverse exit: link parked");

        let mut link = CipherStream::new(
            new_ws_stream(conn, true).await,
            &client_send_cipher,
            &server_send_cipher,
        );
        let idle = REVERSE_LINK_MAX_IDLE - IDLE_JITTER.mul_f64(rand::random());
        let command = match timeout(idle, read_frame::<LinkCommand>(&mut link)).await {
            Err(_) => {
                tracing::debug!(name = self.name, "Reverse exit: replacing an idle link");
                return Ok(());
            }
            Ok(command) => command.context("The server closed the link")?,
        };

        match command {
            LinkCommand::Dial(dial) => {
                tokio::spawn(serve_dial(link, *dial, self.targets.clone()));
            }
        }
        Ok(())
    }
}

/// Connects to the target the server asked for, tells it how that went, and relays.
async fn serve_dial(
    mut link: impl AsyncRead + AsyncWrite + Unpin,
    dial: Dial,
    targets: TargetPolicy,
) {
    let Dial { host, port } = dial;
    tracing::info!(host, port, "Reverse exit: connecting");
    let target = timeout(CONNECT_TIMEOUT, targets.connect(&host, port))
        .await
        .context("Timed out connecting")
        .and_then(|r| r);

    let target = match target {
        Ok(target) => target,
        Err(e) => {
            let msg = format!("{e:#} to {host}:{port}");
            tracing::warn!(msg, "Reverse exit: dial failed");
            let _ = write_frame(&mut link, &DialReply::Failed(Box::new(Failed { msg }))).await;
            return;
        }
    };

    if let Err(e) = write_frame(&mut link, &DialReply::Connected(Box::new(Connected {}))).await {
        tracing::debug!(error = %e, "Reverse exit: link closed before relaying");
        return;
    }
    let _ = cpxy_ng::relay::relay(target, link, cpxy_ng::relay::DEFAULT_LINGER).await;
}
