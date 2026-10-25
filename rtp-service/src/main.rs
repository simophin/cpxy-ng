use rtp_service::{config::Settings, Service};
use std::{
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{net::UnixDatagram, process::CommandExt},
    },
    path::Path,
    process::{Child, Command},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
static SIGNAL: AtomicU8 = AtomicU8::new(0);
extern "C" fn signal(value: i32) {
    SIGNAL.store(if value == libc::SIGHUP { 1 } else { 2 }, Ordering::Relaxed);
}
fn install_signals() {
    unsafe {
        libc::signal(libc::SIGTERM, signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, signal as *const () as libc::sighandler_t);
    }
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
fn parent(path: &Path, settings: &Settings, application: &[String]) -> io::Result<()> {
    let policy = rtp_service::broker::Policy::from_settings(settings)?;
    let (channel, inherited) = UnixDatagram::pair()?;
    let descriptor = inherited.as_raw_fd();
    let mut command = Command::new("unshare");
    command
        .args(["-Urnm"])
        .arg(std::env::current_exe()?)
        .arg("namespace-child")
        .arg(path)
        .arg(descriptor.to_string());
    if !application.is_empty() {
        command.arg("--").args(application);
    }
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
        match SIGNAL.swap(0, Ordering::Relaxed) {
            2 => return Ok(()),
            1 => unsafe {
                libc::kill(child.0.id() as i32, libc::SIGHUP);
            },
            _ => {}
        }
        policy.serve_once(&channel)?;
    }
}
fn run(
    path: &Path,
    settings: Settings,
    broker: Option<File>,
    application: &[String],
) -> io::Result<()> {
    let events = Arc::new(|event| eprintln!("{event}"));
    let isolated = broker.is_some();
    let initial_profile = serde_json::to_value(&settings)?;
    let mut service = match broker {
        Some(broker) => Service::new_with_socket_broker(settings, events, broker)?,
        None => Service::new(settings, events)?,
    };
    service.start()?;
    let mut app: Option<Children> = None;
    let deadline = Instant::now() + Duration::from_secs(90);
    let result = (|| {
        while service.is_running() {
            match SIGNAL.swap(0, Ordering::Relaxed) {
                2 => return Ok(()),
                1 => {
                    let reload = Settings::load(path).and_then(|new| {
                        if isolated && serde_json::to_value(&new)? != initial_profile {
                            return Err(io::Error::other(
                                "namespace profile changes require restart",
                            ));
                        }
                        service.reload(new)
                    });
                    if let Err(error) = reload {
                        eprintln!(
                            "{}",
                            serde_json::json!({"event":"reload_failed","error":error.to_string()})
                        );
                    }
                }
                _ => {}
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
fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 3 {
        return Err(io::Error::other("usage: cpxy-rtp run|doctor|cleanup|cleanup-ports PROFILE.json | namespace-run PROFILE.json [-- COMMAND... ]"));
    }
    let path = Path::new(&args[2]);
    let settings = Settings::load(path)?;
    install_signals();
    match args[1].as_str() {
        "namespace-run" => {
            if args.len() > 3 && (args[3] != "--" || args.len() == 4) {
                return Err(io::Error::other("expected -- COMMAND..."));
            }
            parent(path, &settings, args.get(4..).unwrap_or_default())?;
        }
        "namespace-child" => {
            let descriptor: i32 = args
                .get(3)
                .ok_or_else(|| io::Error::other("missing broker descriptor"))?
                .parse()
                .map_err(|_| io::Error::other("invalid broker descriptor"))?;
            if descriptor < 3 || (args.len() > 4 && (args[4] != "--" || args.len() == 5)) {
                return Err(io::Error::other("invalid namespace child arguments"));
            }
            // Restore CLOEXEC before any ip/application subprocess is created.
            if unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
            let broker = unsafe { File::from_raw_fd(descriptor) };
            run(
                path,
                settings,
                Some(broker),
                args.get(5..).unwrap_or_default(),
            )?;
        }
        action if args.len() == 3 => match action {
            "doctor" => println!("{}", rtp_service::doctor(&settings)),
            "cleanup-ports" => rtp_service::cleanup_ports(&settings)?,
            "cleanup" => rtp_service::cleanup(&settings)?,
            "run" => run(path, settings, None, &[])?,
            _ => return Err(io::Error::other("unknown command")),
        },
        _ => return Err(io::Error::other("unexpected arguments")),
    }
    Ok(())
}
