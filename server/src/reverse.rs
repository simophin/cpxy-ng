//! Reverse exits: machines that can't accept connections (`cpxy reverse-proxy`) keep links
//! parked at the server, and requests routed to `reverse://<name>` leave through them.
//!
//! A link is a connection the exit opened with [`ClientMessage::Register`]. It waits here until
//! a request needs it; the server then sends a [`LinkCommand::Dial`], the exit connects to the
//! target and answers with a [`DialReply`], and from then on the link carries that one tunnel.
//! The exit replaces each link as soon as it's used, and every
//! [`REVERSE_LINK_MAX_IDLE`](cpxy_ng::protocol::REVERSE_LINK_MAX_IDLE) while idle.
//!
//! [`ClientMessage::Register`]: cpxy_ng::protocol::ClientMessage::Register

use anyhow::{Context, bail, format_err};
use cpxy_ng::protocol::{
    Dial, DialReply, LinkCommand, REVERSE_LINK_MAX_IDLE, read_frame, write_frame,
};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;
use tokio::time::{Instant, timeout, timeout_at};

pub trait LinkStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> LinkStream for T {}

/// A registered link, already speaking the tunnel's ciphers.
pub type Link = Box<dyn LinkStream>;

/// Parked links older than this are dropped rather than used: the exit has replaced them by
/// now, so they are either closed or dead.
const MAX_PARKED: Duration = REVERSE_LINK_MAX_IDLE.saturating_add(Duration::from_secs(5));
/// More parked links than this per exit means the exit is misbehaving: the oldest are dropped.
const MAX_LINKS: usize = 64;
/// How long a request waits for a link when none is parked, e.g. while the exit replaces the
/// ones just used.
const WAIT_FOR_LINK: Duration = Duration::from_secs(5);
/// How long the exit may take to connect to the target and answer.
const DIAL_REPLY_TIMEOUT: Duration = Duration::from_secs(15);

/// The reverse exits named by the server's routes, with their parked links.
#[derive(Default)]
pub struct ReverseExits {
    exits: HashMap<String, Exit>,
}

#[derive(Default)]
struct Exit {
    links: Mutex<Vec<(Link, Instant)>>,
    parked: Notify,
}

impl Debug for ReverseExits {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.exits.keys()).finish()
    }
}

impl ReverseExits {
    /// Accepts links only for these names.
    pub fn new<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            exits: names
                .into_iter()
                .map(|name| (name.to_string(), Exit::default()))
                .collect(),
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.exits.contains_key(name)
    }

    /// Keeps `link` until a request routed to `name` needs it.
    pub fn park(&self, name: &str, link: Link) -> anyhow::Result<()> {
        let exit = self
            .exits
            .get(name)
            .with_context(|| format!("No reverse:// route names {name:?}"))?;
        {
            let mut links = exit.links.lock().unwrap();
            links.retain(|(_, since)| since.elapsed() < MAX_PARKED);
            if links.len() >= MAX_LINKS {
                links.remove(0);
            }
            links.push((link, Instant::now()));
        }
        exit.parked.notify_one();
        Ok(())
    }

    /// Connects to `host:port` through the exit `name`, returning the link once the exit has
    /// connected. Links that turn out dead are skipped.
    pub async fn dial(&self, name: &str, host: &str, port: u16) -> anyhow::Result<Link> {
        let exit = self
            .exits
            .get(name)
            .with_context(|| format!("No reverse:// route names {name:?}"))?;
        let deadline = Instant::now() + WAIT_FOR_LINK;
        loop {
            let link = exit.take(deadline).await.ok_or_else(|| {
                format_err!("Reverse exit {name:?} has no link: is it running and registered?")
            })?;
            match offer(link, host, port).await {
                Ok(Some(link)) => return Ok(link),
                Ok(None) => {
                    tracing::debug!(name, "Reverse exit: parked link was dead, trying another")
                }
                Err(e) => return Err(e.context(format!("Reverse exit {name:?}"))),
            }
        }
    }
}

impl Exit {
    /// The most recently parked live link, waiting until `deadline` for one if there are none.
    async fn take(&self, deadline: Instant) -> Option<Link> {
        loop {
            // Register for the wakeup before looking, so a link parked in between isn't missed.
            let parked = self.parked.notified();
            tokio::pin!(parked);
            parked.as_mut().enable();

            {
                let mut links = self.links.lock().unwrap();
                links.retain(|(_, since)| since.elapsed() < MAX_PARKED);
                if let Some((link, _)) = links.pop() {
                    return Some(link);
                }
            }

            timeout_at(deadline, parked).await.ok()?;
        }
    }
}

/// Asks the exit on `link` to connect to `host:port`. `Ok(None)` means the link was dead before
/// the exit could answer, so another link may work.
async fn offer(mut link: Link, host: &str, port: u16) -> anyhow::Result<Option<Link>> {
    let dial = LinkCommand::Dial(Box::new(Dial {
        host: host.to_string(),
        port,
    }));
    if write_frame(&mut link, &dial).await.is_err() {
        return Ok(None);
    }

    match timeout(DIAL_REPLY_TIMEOUT, read_frame::<DialReply>(&mut link)).await {
        Err(_) => bail!("no answer to the dial within {DIAL_REPLY_TIMEOUT:?}"),
        Ok(Err(_)) => Ok(None),
        Ok(Ok(DialReply::Connected(_))) => Ok(Some(link)),
        Ok(Ok(DialReply::Failed(failed))) => bail!("{}", failed.msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpxy_ng::protocol::{Connected, Failed};
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};

    /// A link whose exit answers the first dial with `reply` and then echoes.
    fn link_answering(reply: DialReply) -> (Link, tokio::task::JoinHandle<Dial>) {
        let (server_end, mut exit_end) = duplex(1024);
        let exit = tokio::spawn(async move {
            let LinkCommand::Dial(dial) = read_frame(&mut exit_end).await.unwrap();
            write_frame(&mut exit_end, &reply).await.unwrap();
            let (mut r, mut w) = tokio::io::split(exit_end);
            let _ = tokio::io::copy(&mut r, &mut w).await;
            *dial
        });
        (Box::new(server_end), exit)
    }

    fn dead_link() -> Link {
        let (server_end, exit_end): (DuplexStream, _) = duplex(1024);
        drop(exit_end);
        Box::new(server_end)
    }

    fn connected() -> DialReply {
        DialReply::Connected(Box::new(Connected {}))
    }

    #[tokio::test]
    async fn dials_through_a_parked_link() {
        let exits = ReverseExits::new(["home"]);
        let (link, exit) = link_answering(connected());
        exits.park("home", link).unwrap();

        let mut tunnel = exits.dial("home", "example.com", 443).await.unwrap();
        let dial = exit_dial_of(exit, &mut tunnel).await;
        assert_eq!((dial.host.as_str(), dial.port), ("example.com", 443));
    }

    /// Writes through the tunnel, checks the echo, then returns what the exit was asked.
    async fn exit_dial_of(exit: tokio::task::JoinHandle<Dial>, tunnel: &mut Link) -> Dial {
        tunnel.write_all(b"ping").await.unwrap();
        let mut echo = [0; 4];
        tunnel.read_exact(&mut echo).await.unwrap();
        assert_eq!(&echo, b"ping");
        tunnel.shutdown().await.unwrap();
        exit.await.unwrap()
    }

    #[tokio::test]
    async fn skips_dead_links() {
        let exits = ReverseExits::new(["home"]);
        let (link, exit) = link_answering(connected());
        exits.park("home", link).unwrap();
        // Parked last, so taken first.
        exits.park("home", dead_link()).unwrap();

        let mut tunnel = exits.dial("home", "example.com", 80).await.unwrap();
        exit_dial_of(exit, &mut tunnel).await;
    }

    #[tokio::test]
    async fn reports_the_exits_failure() {
        let exits = ReverseExits::new(["home"]);
        let (link, _exit) = link_answering(DialReply::Failed(Box::new(Failed {
            msg: "connection refused".into(),
        })));
        exits.park("home", link).unwrap();

        let err = exits.dial("home", "example.com", 80).await.err().unwrap();
        assert!(format!("{err:#}").contains("connection refused"), "{err:#}");
    }

    #[tokio::test]
    async fn waits_for_a_link_to_be_parked() {
        let exits = std::sync::Arc::new(ReverseExits::new(["home"]));
        let dialing = tokio::spawn({
            let exits = exits.clone();
            async move { exits.dial("home", "example.com", 80).await.map(|_| ()) }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (link, _exit) = link_answering(connected());
        exits.park("home", link).unwrap();
        dialing.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_without_links() {
        let exits = ReverseExits::new(["home"]);
        let err = exits.dial("home", "example.com", 80).await.err().unwrap();
        assert!(err.to_string().contains("has no link"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn drops_expired_links() {
        let exits = ReverseExits::new(["home"]);
        let (link, _exit) = link_answering(connected());
        exits.park("home", link).unwrap();
        tokio::time::advance(MAX_PARKED).await;
        assert!(exits.dial("home", "example.com", 80).await.is_err());
    }

    #[test]
    fn only_named_exits_take_links() {
        let exits = ReverseExits::new(["home"]);
        assert!(exits.contains("home"));
        assert!(!exits.contains("other"));
        assert!(exits.park("other", dead_link()).is_err());
    }
}
