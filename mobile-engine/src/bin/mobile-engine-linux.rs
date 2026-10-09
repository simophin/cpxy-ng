//! Runs the engine on a Linux TUN device, standing in for the phone in the integration test
//! (`mobile-engine/test/tun-lab.sh`).
//!
//! usage: mobile-engine-linux <tun name> <config json file>
//!
//! The device is created down and without addresses; whoever runs this configures it, and may
//! move it to another network namespace so that the engine's own sockets bypass it, as the app's
//! do on the phone.

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use anyhow::Context;
    use mobile_engine::{EventListener, OutboundEvent};
    use std::sync::Arc;

    struct LogListener;
    impl EventListener for LogListener {
        fn on_outbound_event(&self, event: OutboundEvent) {
            println!("event: {}", serde_json::to_string(&event).unwrap());
        }
    }

    tracing_subscriber::fmt::init();
    let mut args = std::env::args().skip(1);
    let (Some(name), Some(config)) = (args.next(), args.next()) else {
        anyhow::bail!("usage: mobile-engine-linux <tun name> <config json file>");
    };
    let config = std::fs::read_to_string(&config).with_context(|| format!("Reading {config}"))?;
    let tun = mobile_engine::linux::create_tun(&name)?;

    let engine = mobile_engine::start(tun, &config, Arc::new(LogListener))?;
    println!("running on {name}");

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = signal(SignalKind::terminate())?;
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            anyhow::Ok(())
        })?;

    let traffic = engine.traffic();
    engine.stop();
    println!(
        "stopped (sent {} bytes, received {} bytes)",
        traffic.sent, traffic.received
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("mobile-engine-linux only runs on Linux");
    std::process::exit(1);
}
