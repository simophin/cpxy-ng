//! Runs the engine on a Linux TUN, standing in for the phone in `tests/linux-lab.sh`.
//!
//! ```text
//! rtp-mobile-linux routes
//! rtp-mobile-linux run PROFILE TUN DEVICE CONTROL_DIR
//! ```
//!
//! `run` takes the password from `CPXY_RTP_PASSWORD`, binds the tunnel's sockets to DEVICE as
//! `VpnService.protect` would keep them off the VPN, and prints its status as JSON every 200 ms.
//! Creating `CONTROL_DIR/repair` reports a network change; `CONTROL_DIR/stop` stops it.

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use std::path::Path;
    use std::sync::Arc;

    struct BindToDevice(std::ffi::CString);
    impl rtp_mobile::Platform for BindToDevice {
        fn protect_socket(&self, fd: std::os::fd::RawFd) -> std::io::Result<()> {
            let name = self.0.as_bytes_with_nul();
            // SAFETY: SO_BINDTODEVICE reads a NUL-terminated interface name.
            let r = unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_BINDTODEVICE,
                    name.as_ptr().cast(),
                    name.len() as _,
                )
            };
            if r < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
    }

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["routes"] => {
            for route in rtp_mobile::tunnel_routes() {
                println!("{route}");
            }
            Ok(())
        }
        ["run", profile, tun, device, control] => {
            let profile =
                rtp_mobile::profile::Profile::from_json(&std::fs::read_to_string(profile)?)?;
            let password = std::env::var("CPXY_RTP_PASSWORD").context("CPXY_RTP_PASSWORD")?;
            let key = rtp_l3::derive_psk(&password).map_err(anyhow::Error::msg)?;
            std::fs::create_dir_all(control)?;
            let id = rtp_l3::v2::identity::client_id(&Path::new(control).join("client-id"))?;
            let prepared = rtp_mobile::prepare(
                &profile,
                key,
                id,
                Arc::new(BindToDevice(std::ffi::CString::new(device)?)),
            )?;
            let inner = prepared.address();
            let tun_fd = create_tun(tun)?;
            anyhow::ensure!(
                std::process::Command::new("ip")
                    .args(["addr", "replace", &format!("{inner}/32"), "dev", tun])
                    .status()?
                    .success(),
                "Cannot configure assigned address"
            );
            let engine = prepared.start(tun_fd)?;
            let control = Path::new(control);
            while !control.join("stop").exists() {
                if std::fs::remove_file(control.join("repair")).is_ok() {
                    engine.network_changed();
                }
                println!("{}", serde_json::to_string(&engine.status())?);
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            engine.stop();
            println!("{}", serde_json::to_string(&engine.status())?);
            let tun_open = std::fs::read_dir("/proc/self/fd")?
                .filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
                .any(|target| target == Path::new("/dev/net/tun"));
            ensure!(!tun_open, "The TUN is still open after stop");
            Ok(())
        }
        _ => anyhow::bail!("usage: rtp-mobile-linux routes | run PROFILE TUN DEVICE CONTROL_DIR"),
    }
}

#[cfg(target_os = "linux")]
fn create_tun(name: &str) -> anyhow::Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsRawFd;
    #[repr(C)]
    struct IfReq {
        name: [u8; libc::IFNAMSIZ],
        flags: libc::c_short,
        _pad: [u8; 22],
    }
    anyhow::ensure!(name.len() < libc::IFNAMSIZ, "TUN name too long");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")?;
    let mut req = IfReq {
        name: [0; libc::IFNAMSIZ],
        flags: (libc::IFF_TUN | libc::IFF_NO_PI) as libc::c_short,
        _pad: [0; 22],
    };
    req.name[..name.len()].copy_from_slice(name.as_bytes());
    // SAFETY: TUNSETIFF reads and writes an ifreq, which IfReq lays out.
    if unsafe { libc::ioctl(file.as_raw_fd(), 0x400454ca as _, &mut req) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(file.into())
}

#[cfg(not(target_os = "linux"))]
fn main() {}
