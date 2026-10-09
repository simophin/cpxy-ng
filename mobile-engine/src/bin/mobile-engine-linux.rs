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
    let tun = linux::create_tun(&name)?;

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

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{Context, ensure};
    use std::fs::OpenOptions;
    use std::os::fd::{AsRawFd, OwnedFd};

    const TUNSETIFF: libc::c_ulong = 0x400454ca;

    #[repr(C)]
    struct IfReq {
        name: [u8; libc::IFNAMSIZ],
        flags: libc::c_short,
        _pad: [u8; 22],
    }

    pub fn create_tun(name: &str) -> anyhow::Result<OwnedFd> {
        ensure!(name.len() < libc::IFNAMSIZ, "TUN name {name} is too long");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/net/tun")
            .context("Opening /dev/net/tun")?;

        let mut req = IfReq {
            name: [0; libc::IFNAMSIZ],
            flags: (libc::IFF_TUN | libc::IFF_NO_PI) as libc::c_short,
            _pad: [0; 22],
        };
        req.name[..name.len()].copy_from_slice(name.as_bytes());
        // SAFETY: TUNSETIFF reads and writes an ifreq, which IfReq lays out.
        let r = unsafe { libc::ioctl(file.as_raw_fd(), TUNSETIFF as _, &mut req) };
        ensure!(
            r == 0,
            "Creating TUN {name}: {}",
            std::io::Error::last_os_error()
        );
        Ok(file.into())
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("mobile-engine-linux only runs on Linux");
    std::process::exit(1);
}
