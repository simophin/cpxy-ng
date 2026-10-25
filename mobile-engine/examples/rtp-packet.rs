//! Disposable Linux fixture adapter; production platforms implement Platform.
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use mobile_engine::packet::{PacketEngine, PacketProfile, Platform};
    use std::{
        io,
        net::UdpSocket,
        os::fd::{AsRawFd, OwnedFd},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    struct Host {
        key: Vec<u8>,
        device: std::ffi::CString,
        calls: AtomicUsize,
    }
    impl Platform for Host {
        fn resolve_key(&self, _: &str, reference: &str) -> io::Result<[u8; 32]> {
            if reference != "fixture-key" {
                return Err(io::Error::other("Unknown reference"));
            }
            self.key
                .as_slice()
                .try_into()
                .map_err(|_| io::Error::other("Invalid key"))
        }
        fn prepare_network(&self, _: &PacketProfile) -> io::Result<()> {
            Ok(())
        } // fixture owns policy
        fn protect_socket(&self, socket: &UdpSocket) -> io::Result<()> {
            // Pin every initial/candidate/repair socket to the physical device.
            let device = self.device.as_bytes_with_nul();
            let result = unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_BINDTODEVICE,
                    device.as_ptr().cast(),
                    device.len() as _,
                )
            };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }
    let args: Vec<_> = std::env::args().collect();
    let profile: PacketProfile = serde_json::from_str(&std::fs::read_to_string(&args[1])?)?;
    // Physical device for SO_BINDTODEVICE; the namespace fixture names it cr.
    let device = std::env::var("RTP_PROTECT_DEVICE").unwrap_or_else(|_| "cr".into());
    profile.validate()?;
    let fd = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")?;
    let mut request = [0u8; 40];
    request[..4].copy_from_slice(b"rtpc");
    request[16..18].copy_from_slice(&((libc::IFF_TUN | libc::IFF_NO_PI) as i16).to_ne_bytes());
    if unsafe {
        libc::ioctl(
            fd.as_raw_fd(),
            0x400454ca as libc::c_ulong,
            request.as_mut_ptr(),
        )
    } < 0
    {
        return Err(io::Error::last_os_error().into());
    }
    let host = Arc::new(Host {
        key: std::fs::read(&args[2])?,
        device: std::ffi::CString::new(device)?,
        calls: AtomicUsize::new(0),
    });
    let engine = PacketEngine::start(OwnedFd::from(fd), profile.clone(), host.clone())?;
    while !std::path::Path::new(&args[3]).exists() {
        println!("{}", serde_json::to_string(&engine.diagnostics())?);
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    engine.stop();
    engine.stop();
    println!("{}", serde_json::to_string(&engine.diagnostics())?);
    println!("protected_sockets={}", host.calls.load(Ordering::Relaxed));
    assert!(host.calls.load(Ordering::Relaxed) >= 2);
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() {}
