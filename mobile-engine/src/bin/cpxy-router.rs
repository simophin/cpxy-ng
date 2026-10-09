//! Native OpenWrt TUN client; routing and device persistence belong to the init script.
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use clap::Parser;
    use mobile_engine::{EventListener, OutboundEvent};
    use std::sync::Arc;

    #[derive(Parser)]
    struct Options {
        #[arg(long, default_value = "cpxy0")]
        tun: String,
        /// The engine sizes TCP segments to this and ignores the peer's MSS, so it must not
        /// exceed the smallest MTU on the path back to clients (1280 behind tailscale0).
        #[arg(long, default_value_t = mobile_engine::DEFAULT_MTU)]
        mtu: u16,
        // Keep the secret out of process arguments and parser diagnostics.
    }
    struct Listener;
    impl EventListener for Listener {
        fn on_outbound_event(&self, _: OutboundEvent) {}
    }
    tracing_subscriber::fmt::init();
    let options = Options::parse();
    let server = std::env::var("SERVER")
        .map_err(|_| anyhow::anyhow!("SERVER is required"))?
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid SERVER URL"))?;
    let tun = mobile_engine::linux::create_tun(&options.tun)?;
    let engine = mobile_engine::start_router(tun, server, options.mtu, Arc::new(Listener))?;
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = signal(SignalKind::terminate())?;
            loop {
                tokio::select! {
                    _ = term.recv() => return anyhow::Ok(()),
                    _ = tokio::signal::ctrl_c() => return Ok(()),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                        anyhow::ensure!(engine.is_running(), "Packet engine stopped");
                    }
                }
            }
        });
    engine.stop();
    result
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cpxy-router only runs on Linux");
    std::process::exit(1);
}
