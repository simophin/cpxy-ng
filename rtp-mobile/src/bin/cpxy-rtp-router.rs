//! Router packet/DNS engine for HTTP-controlled RTP; host policy is owned by procd.
use anyhow::{Context, ensure};
use clap::Parser;
use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    os::fd::{AsRawFd, OwnedFd, RawFd},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
static RUN: AtomicBool = AtomicBool::new(true);
static REPAIR: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    RUN.store(false, Ordering::Relaxed);
}
extern "C" fn repair(_: i32) {
    REPAIR.store(true, Ordering::Relaxed);
}
#[derive(Parser)]
struct Args {
    #[arg(long, required_unless_present_any = ["export_direct_routes", "export_direct_routes6"])]
    entry: Option<String>,
    #[arg(long, default_value = "rtpr0")]
    interface: String,
    #[arg(long, default_value = "/etc/cpxy-rtp/state/main")]
    state_dir: PathBuf,
    #[arg(long)]
    outbound_interface: Option<String>,
    #[arg(long, default_value = "0.0.0.0:5353")]
    dns_listen: SocketAddr,
    #[arg(long, value_delimiter = ',', default_value = "223.5.5.5")]
    dns_upstream: Vec<String>,
    #[arg(long, value_delimiter = ',', default_value = "8.8.8.8,1.1.1.1")]
    dns_alternative: Vec<Ipv4Addr>,
    #[arg(long, default_value = "127.0.0.1:53")]
    dns_local_server: String,
    #[arg(long, value_delimiter = ',', default_value = "lan")]
    dns_local_domain: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    dns_search_domain: Vec<String>,
    #[arg(long)]
    export_direct_routes: bool,
    #[arg(long)]
    export_direct_routes6: bool,
    /// Advertise only regional AAAA; host policy forwards permitted IPv6 outside RTP.
    #[arg(long)]
    ipv6_direct: bool,
    /// Validate configuration without opening sockets or changing network state.
    #[arg(long)]
    check: bool,
    #[arg(long, value_delimiter = ',')]
    proxy_route: Vec<ipnet::Ipv4Net>,
}
struct Platform(Option<std::ffi::CString>);
impl rtp_mobile::Platform for Platform {
    fn protect_socket(&self, fd: RawFd) -> std::io::Result<()> {
        if let Some(device) = &self.0 {
            if unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_BINDTODEVICE,
                    device.as_ptr().cast(),
                    device.as_bytes_with_nul().len() as _,
                )
            } < 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }
}
fn ip(args: &[&str]) -> anyhow::Result<()> {
    let output = std::process::Command::new("ip").args(args).output()?;
    ensure!(
        output.status.success(),
        "ip setup failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}
fn tun(name: &str, address: Ipv4Addr) -> anyhow::Result<OwnedFd> {
    ensure!(
        !name.is_empty()
            && name.len() < libc::IFNAMSIZ
            && name.bytes().all(|b| b.is_ascii_alphanumeric()),
        "Invalid TUN name"
    );
    ensure!(
        !std::process::Command::new("ip")
            .args(["link", "show", "dev", name])
            .output()?
            .status
            .success(),
        "TUN name already exists"
    );
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")?;
    let mut req = [0u8; 40];
    req[..name.len()].copy_from_slice(name.as_bytes());
    req[16..18].copy_from_slice(&0x9001u16.to_ne_bytes());
    if unsafe { libc::ioctl(file.as_raw_fd(), 0x400454ca as _, req.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    ip(&["addr", "add", &format!("{address}/32"), "dev", name])?;
    ip(&["link", "set", "dev", name, "mtu", "1200", "up"])?;
    Ok(file.into())
}
fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.export_direct_routes {
        let exclusions: Vec<_> = args
            .proxy_route
            .iter()
            .map(|n| (u32::from(n.network()), u32::from(n.broadcast())))
            .collect();
        for (ip, prefix) in geoip_data::routes::direct_ipv4(&exclusions) {
            println!("{}/{prefix}", Ipv4Addr::from(ip));
        }
        return Ok(());
    }
    if args.export_direct_routes6 {
        for (ip, prefix) in geoip_data::routes::direct_ipv6() {
            println!("{}/{prefix}", Ipv6Addr::from(ip));
        }
        return Ok(());
    }
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGUSR1, repair as *const () as libc::sighandler_t);
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let password = std::env::var("CPXY_RTP_PASSWORD")
        .context("CPXY_RTP_PASSWORD must hold the shared password")?;
    unsafe {
        std::env::remove_var("CPXY_RTP_PASSWORD");
    }
    let key = rtp_l3::derive_psk(&password).map_err(anyhow::Error::msg)?;
    let profile = rtp_mobile::profile::Profile {
        endpoint: args.entry.unwrap(),
        dns_upstream: args.dns_upstream,
        dns_alternative: args.dns_alternative,
        dns_local_server: Some(args.dns_local_server),
        dns_local_domains: args.dns_local_domain,
        dns_search_domains: args.dns_search_domain,
    };
    profile.settings()?;
    if args.check {
        return Ok(());
    }
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&args.state_dir)?;
    let id = rtp_l3::v2::identity::client_id(&args.state_dir.join("client-id"))?;
    let device = args
        .outbound_interface
        .map(std::ffi::CString::new)
        .transpose()?;
    let prepared = rtp_mobile::prepare(&profile, key, id, Arc::new(Platform(device)))?;
    let address = prepared.address();
    let tun = tun(&args.interface, address)?;
    let engine = if args.ipv6_direct {
        prepared.start_direct_ipv6(tun)?
    } else {
        prepared.start(tun)?
    };
    engine.serve_dns(args.dns_listen)?;
    eprintln!(
        "{}",
        serde_json::json!({"event":"ready","inner":address,"interface":args.interface,"protocol":"rtp-http-v2"})
    );
    let mut shown = None;
    let mut reported = std::time::Instant::now();
    while RUN.load(Ordering::Relaxed) {
        if REPAIR.swap(false, Ordering::Relaxed) {
            engine.network_changed();
        }
        let status = engine.status();
        if shown != Some(status.state) || reported.elapsed() >= Duration::from_secs(10) {
            println!("{}", serde_json::to_string(&status)?);
            shown = Some(status.state);
            reported = std::time::Instant::now();
        }
        if matches!(
            status.state,
            rtp_mobile::State::Failed | rtp_mobile::State::Stopped
        ) {
            anyhow::bail!("RTP client stopped");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    engine.stop();
    Ok(())
}
