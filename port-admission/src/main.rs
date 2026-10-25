use port_admission::{
    agent::{self, Config},
    firewalld::{DbusFirewall, ManagedFirewall},
};
use std::{
    io,
    os::unix::{fs::PermissionsExt, net::UnixListener},
};
fn main() -> io::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| io::Error::other("usage: port-admission AGENT.json"))?;
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    let firewall = ManagedFirewall::open(config.policies, DbusFirewall, config.journal)?;
    // Never unlink a socket owned by an existing agent. On restart the service
    // manager removes its private RuntimeDirectory/socket after process exit.
    let listener = UnixListener::bind(config.socket.clone())?;
    std::fs::set_permissions(&config.socket, std::fs::Permissions::from_mode(0o600))?;
    use std::os::unix::ffi::OsStrExt;
    let socket_path =
        std::ffi::CString::new(config.socket.as_os_str().as_bytes()).map_err(io::Error::other)?;
    if unsafe { libc::chown(socket_path.as_ptr(), config.allowed_uid, u32::MAX) } != 0 {
        return Err(io::Error::last_os_error());
    }
    agent::serve(listener, config.allowed_uid, &firewall)
}
