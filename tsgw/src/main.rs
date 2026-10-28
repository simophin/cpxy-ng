//! cpxy-tsgw is a Tailscale exit node that sends its clients' traffic through a cpxy server,
//! entirely in user space: tailscale-rs is the Tailscale node, and the packets clients send to
//! the internet go over a socketpair to the packet engine (mobile-engine), which proxies TCP,
//! relays UDP and answers DNS. Clients' DNS over the peer API is answered by the same engine. See
//! docs/cpxy-tsgw.md.

#[cfg(target_os = "linux")]
mod bridge;
#[cfg(target_os = "linux")]
mod dns;
#[cfg(target_os = "linux")]
mod settings;
#[cfg(target_os = "linux")]
mod state;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    match linux::main() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cpxy-tsgw: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("cpxy-tsgw runs on Linux only")
}

#[cfg(target_os = "linux")]
mod linux {
    use crate::settings::{self, Settings};
    use crate::{bridge, dns};
    use anyhow::Context;
    use clap::Parser;
    use mobile_engine::EngineHandle;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use tailscale::{Config, Device, DeviceState};
    use tokio::signal::unix::{SignalKind, signal};

    /// How often the node's state is checked, and so how soon a change shows in the log.
    const STATUS_INTERVAL: Duration = Duration::from_secs(5);
    const ENGINE_CHECK: Duration = Duration::from_secs(1);
    const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
    /// The peer API's port on the node's own Tailscale address, which only peers reach. Clients
    /// that use the exit node send their DNS there.
    const PEERAPI_PORT: u16 = 41643;

    #[derive(Parser)]
    #[command(
        about = "A Tailscale exit node that sends its clients' traffic through a cpxy server"
    )]
    struct Flags {
        /// A file of KEY=value settings; the environment overrides it.
        #[arg(long)]
        config: Option<PathBuf>,
        /// The node's name on the tailnet (TS_HOSTNAME, default cpxy-tsgw).
        #[arg(long)]
        hostname: Option<String>,
        /// Where the node keeps its Tailscale state (STATE_DIR).
        #[arg(long)]
        state_dir: Option<String>,
        /// The coordination server, e.g. a Headscale URL (CONTROL_URL).
        #[arg(long)]
        control_url: Option<String>,
        /// Log Tailscale's own debug output.
        #[arg(long)]
        verbose: bool,
    }

    pub fn main() -> anyhow::Result<()> {
        // tailscale-rs refuses to start without this acknowledgement that it is pre-release
        // software, which cpxy-tsgw makes for its users.
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("TS_RS_EXPERIMENT", "this_is_unstable_software") };
        let flags = Flags::parse();
        init_logging(flags.verbose);

        let file = match &flags.config {
            Some(path) => settings::read_config_file(path)?,
            None => Default::default(),
        };
        let mut s = settings::load(&file, |k| std::env::var(k).ok())?;
        for (flag, setting) in [
            (flags.hostname, &mut s.hostname),
            (flags.state_dir, &mut s.state_dir),
            (flags.control_url, &mut s.control_url),
        ] {
            if let Some(v) = flag.filter(|v| !v.is_empty()) {
                *setting = v;
            }
        }
        if s.state_dir.is_empty() {
            anyhow::bail!("STATE_DIR is required: where the node keeps its Tailscale state");
        }

        let (ours, engine_fd) = bridge::socketpair()?;
        // The engine runs on its own runtime, and blocks while starting and stopping: both happen
        // here, outside ours.
        let engine = Arc::new(
            mobile_engine::start_gateway(
                engine_fd,
                &serde_json::to_string(&s.engine)?,
                Arc::new(()),
            )
            .context("starting the packet engine")?,
        );
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .thread_name("cpxy-tsgw")
            .enable_all()
            .build()?;
        let result = runtime.block_on(run(s, ours, engine.clone()));
        runtime.shutdown_timeout(SHUTDOWN_TIMEOUT);
        engine.stop();
        result
    }

    fn init_logging(verbose: bool) {
        use tracing_subscriber::EnvFilter;
        // tailscale-rs logs at info what Go's tsnet logs at its debug level.
        let default = if verbose {
            "info,cpxy_tsgw=debug"
        } else {
            "warn,cpxy_tsgw=info,mobile_engine=info,client=info"
        };
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default)),
            )
            .with_ansi(false)
            .with_writer(std::io::stderr)
            .with_target(verbose)
            .init();
    }

    async fn run(
        s: Settings,
        ours: std::os::fd::OwnedFd,
        engine: Arc<EngineHandle>,
    ) -> anyhow::Result<()> {
        let dns = dns::serve(engine.clone()).await?;

        let state_dir = PathBuf::from(&s.state_dir);
        tokio::fs::create_dir_all(&state_dir)
            .await
            .with_context(|| format!("creating {}", state_dir.display()))?;
        match crate::state::import_go_state(&state_dir) {
            Ok(true) => {
                tracing::info!("Carried the node's Tailscale identity over from the Go cpxy-tsgw")
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(
                "Not carrying the Go cpxy-tsgw's identity over, so this is a new node: {e:#}"
            ),
        }
        let key_state = tailscale::config::load_key_file(
            state_dir.join(crate::state::KEY_FILE),
            tailscale::config::BadFormatBehavior::Error,
        )
        .await?;
        let mut config = Config {
            key_state,
            client_name: Some("cpxy-tsgw".to_owned()),
            requested_hostname: Some(s.hostname.clone()),
            ephemeral: false,
            // The tailnet's DNS config answers the tailnet's own names over the peer API.
            accept_dns: true,
            advertise_exit_node: true,
            // Lets the peer API forward clients' DNS to `dns`; the traffic itself is ours.
            forward_exit_egress: true,
            exit_node_dns: Some(dns),
            peerapi_port: Some(PEERAPI_PORT),
            enable_ipv6: true,
            external_forwarder: true,
            // A fixed port lets a router forward it, so peers connect directly more often.
            wireguard_listen_port: (s.port != 0).then_some(s.port),
            ..Config::default()
        };
        if !s.control_url.is_empty() {
            let url: url::Url = s.control_url.parse().context("invalid CONTROL_URL")?;
            // Asked for in the URL, e.g. a Headscale on the LAN: fetch the server's key over it
            // too, rather than over https.
            config.allow_http_key_fetch = url.scheme() == "http";
            config.control_server_url = url;
        }
        // For the benchmark only: also route these, e.g. this host's private address, which an
        // exit node alone refuses.
        if let Ok(extra) = std::env::var("CPXY_TSGW_EXTRA_ROUTES") {
            for r in extra.split(',').map(str::trim).filter(|r| !r.is_empty()) {
                config.advertise_routes.push(
                    r.parse()
                        .with_context(|| format!("CPXY_TSGW_EXTRA_ROUTES: {r}"))?,
                );
            }
        }

        tracing::info!(
            "Starting Tailscale as {:?}; on the first run a login link appears here",
            s.hostname
        );
        let device = Device::new(&config, s.auth_key.clone())
            .await
            .context("starting Tailscale")?;
        let packets = device
            .take_forwarded_packets()
            .context("Tailscale did not hand over the forwarded packets")?;
        let mtu = s.engine.mtu;
        let mut bridge = tokio::spawn(bridge::run(ours, packets, mtu));

        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;
        let mut status = tokio::time::interval(STATUS_INTERVAL);
        let mut engine_check = tokio::time::interval(ENGINE_CHECK);
        let mut state = device.watch_state();
        let mut reporter = Reporter::default();
        let result = loop {
            tokio::select! {
                _ = sigterm.recv() => break Ok(()),
                _ = sigint.recv() => break Ok(()),
                r = &mut bridge => break Err(match r {
                    Ok(Err(e)) => e,
                    Ok(Ok(())) => anyhow::anyhow!("the packet bridge stopped"),
                    Err(e) => e.into(),
                }),
                _ = engine_check.tick() => if !engine.is_running() {
                    // Exit so the service manager restarts us, rather than black-holing traffic.
                    break Err(anyhow::anyhow!("the packet engine stopped"));
                },
                changed = state.changed() => {
                    if changed.is_err() {
                        break Err(anyhow::anyhow!("Tailscale stopped"));
                    }
                    let current = state.borrow_and_update().clone();
                    if let DeviceState::Failed(e) = current {
                        break Err(anyhow::anyhow!("Tailscale: {e}"));
                    }
                    reporter.report(&device, &engine).await;
                }
                _ = status.tick() => reporter.report(&device, &engine).await,
            }
        };
        tracing::info!("Stopping");
        bridge.abort();
        device.shutdown(Some(SHUTDOWN_TIMEOUT)).await;
        result
    }

    /// Logs the node's state when it changes: its addresses and whether the exit node is
    /// approved yet, or the link to log in with.
    #[derive(Default)]
    struct Reporter {
        last: String,
    }

    impl Reporter {
        async fn report(&mut self, device: &Device, engine: &EngineHandle) {
            let line = match device.device_state() {
                DeviceState::NeedsLogin(url) => {
                    format!(
                        "Log in by opening {url} (waiting; set TS_AUTHKEY to log in without a browser)"
                    )
                }
                DeviceState::Expired => {
                    "Tailscale's node key expired: log in again. Clients are cut off until then"
                        .to_owned()
                }
                DeviceState::Running => {
                    // Approval belongs to the control runner's self node. A full status
                    // query also waits on peer/path actors and can hide a fresh approval
                    // behind unrelated peer discovery until this deadline expires.
                    match tokio::time::timeout(STATUS_INTERVAL, device.self_node()).await {
                        Ok(Ok(node)) => {
                            let me = tailscale::StatusNode::from_node(&node);
                            tracing::debug!(routes = ?me.allowed_routes, "Reading exit-node approval");
                            let approved = me
                                .allowed_routes
                                .iter()
                                .any(|r| r.prefix_len() == 0 && r.addr().is_ipv4());
                            let ips: Vec<_> =
                                me.tailscale_ips.iter().map(|ip| ip.to_string()).collect();
                            let mut line = format!(
                                "Tailscale is Running as {} ({})",
                                me.display_name,
                                ips.join(", ")
                            );
                            line += if approved {
                                "; the exit node is approved"
                            } else {
                                "; the exit node is NOT approved yet: approve it in the admin console (Edit route settings, Use as exit node)"
                            };
                            line
                        }
                        Ok(Err(e)) => format!("Reading Tailscale status: {e}"),
                        Err(_) => {
                            tracing::warn!("Timed out reading Tailscale self node");
                            return;
                        }
                    }
                }
                other => format!("Tailscale is {other:?}"),
            };
            if line != self.last {
                let traffic = engine.traffic();
                tracing::info!(
                    "{line}. Traffic so far: {} bytes from clients, {} to them",
                    traffic.sent,
                    traffic.received
                );
                self.last = line;
            }
        }
    }
}
