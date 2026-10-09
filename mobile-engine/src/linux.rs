//! Linux TUN attachment shared by the router and mobile integration harness.
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
    ensure!(
        !name.is_empty() && name.len() < libc::IFNAMSIZ && !name.as_bytes().contains(&0),
        "Invalid TUN device name"
    );
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
