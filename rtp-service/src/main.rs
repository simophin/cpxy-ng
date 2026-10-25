use clap::Parser;
use rtp_service::{
    cli::{Action, Cli, ClientAction, RoleCommand},
    config::Settings,
    Service, PASSWORD_ENV,
};
use std::{
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{net::UnixDatagram, process::CommandExt},
    },
    process::{Child, Command},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
static SIGNAL: AtomicU8 = AtomicU8::new(0);
/// SIGHUP stops gracefully too: settings come from argv/env, so there is
/// nothing to reload, and the default action would skip cleanup.
extern "C" fn signal(_: i32) {
    SIGNAL.store(2, Ordering::Relaxed);
}
fn install_signals() {
    unsafe {
        libc::signal(libc::SIGTERM, signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, signal as *const () as libc::sighandler_t);
    }
}
/// Internal parent-to-child handoff of already validated settings.
const SETTINGS_ENV: &str = "CPXY_RTP_INTERNAL_SETTINGS";
/// Reads and removes the password so applications never inherit it.
fn psk() -> io::Result<[u8; 32]> {
    let password = std::env::var(PASSWORD_ENV)
        .map_err(|_| io::Error::other(format!("{PASSWORD_ENV} must hold the shared password")))?;
    std::env::remove_var(PASSWORD_ENV);
    rtp_l3::derive_psk(&password).map_err(io::Error::other)
}
fn events() -> rtp_l3::linux::Events {
    Arc::new(|event| eprintln!("{event}"))
}
/// A separate process group contains the app and its descendants. Always reap
/// the direct child, including errors and startup failures.
struct Children(Child);
impl Drop for Children {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.0.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}
fn parent(settings: &Settings, application: &[String]) -> io::Result<()> {
    let policy = rtp_service::broker::Policy::from_settings(settings)?;
    let (channel, inherited) = UnixDatagram::pair()?;
    let descriptor = inherited.as_raw_fd();
    let mut command = Command::new("unshare");
    command
        .args(["-Urnm"])
        .arg(std::env::current_exe()?)
        .args(["namespace-child", "--fd", &descriptor.to_string(), "--"])
        .args(application)
        .env(SETTINGS_ENV, serde_json::to_string(settings)?);
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) < 0 {
                return Err(io::Error::last_os_error());
            }
            // Only this pair endpoint survives exec. No public broker socket.
            if libc::fcntl(descriptor, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = Children(command.spawn()?);
    drop(inherited);
    loop {
        if let Some(status) = child.0.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other("namespace child failed"))
            };
        }
        if SIGNAL.swap(0, Ordering::Relaxed) == 2 {
            return Ok(());
        }
        policy.serve_once(&channel)?;
    }
}
fn run(settings: Settings, broker: Option<File>, application: &[String]) -> io::Result<()> {
    let psk = psk()?;
    let mut service = match broker {
        Some(broker) => Service::new_with_socket_broker(settings, psk, events(), broker)?,
        None => Service::new(settings, psk, events())?,
    };
    service.start()?;
    let mut app: Option<Children> = None;
    let deadline = Instant::now() + Duration::from_secs(90);
    let result = (|| {
        while service.is_running() {
            if SIGNAL.swap(0, Ordering::Relaxed) == 2 {
                return Ok(());
            }
            if !application.is_empty() {
                if app.is_none() && service.health().state == "connected" {
                    let mut command = Command::new(&application[0]);
                    command.args(&application[1..]).process_group(0);
                    unsafe {
                        command.pre_exec(|| {
                            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                                return Err(io::Error::last_os_error());
                            }
                            Ok(())
                        });
                    }
                    app = Some(Children(command.spawn()?));
                }
                if let Some(child) = &mut app {
                    if let Some(status) = child.0.try_wait()? {
                        return if status.success() {
                            Ok(())
                        } else {
                            Err(io::Error::other("namespace application failed"))
                        };
                    }
                } else if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "namespace connection timed out",
                    ));
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(io::Error::other("packet worker stopped unexpectedly"))
    })();
    drop(app);
    let stopped = service.stop();
    result.and(stopped)
}
/// Creates the private state directory; an existing one is used as is.
fn state_dir(settings: &Settings) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = settings
        .journal
        .parent()
        .ok_or_else(|| io::Error::other("state directory"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}
fn action(settings: Settings, action: Action) -> io::Result<()> {
    if action != Action::Doctor {
        state_dir(&settings)?;
    }
    match action {
        Action::Run => run(settings, None, &[]),
        Action::Doctor => {
            println!("{}", rtp_service::doctor(&settings));
            Ok(())
        }
        Action::Cleanup => rtp_service::cleanup(&settings, &events()),
        Action::CleanupPorts => rtp_service::cleanup_ports(&settings, &events()),
    }
}
fn main() -> io::Result<()> {
    install_signals();
    match Cli::parse().role {
        RoleCommand::Endpoint { args, action: a } => {
            action(args.settings()?, a.unwrap_or(Action::Run))
        }
        RoleCommand::Client { args, action: a } => {
            let settings = args.settings()?;
            match a.unwrap_or(ClientAction::Common(Action::Run)) {
                ClientAction::Common(a) => action(settings, a),
                ClientAction::Namespace { command } => {
                    state_dir(&settings)?;
                    parent(&settings, &command)
                }
            }
        }
        RoleCommand::NamespaceChild { fd, command } => {
            let settings: Settings = serde_json::from_str(
                &std::env::var(SETTINGS_ENV)
                    .map_err(|_| io::Error::other("missing namespace settings"))?,
            )?;
            std::env::remove_var(SETTINGS_ENV);
            settings.validate()?;
            if fd < 3 {
                return Err(io::Error::other("invalid broker descriptor"));
            }
            // Restore CLOEXEC before any ip/application subprocess is created.
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
            let broker = unsafe { File::from_raw_fd(fd) };
            run(settings, Some(broker), &command)
        }
    }
}
