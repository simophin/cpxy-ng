use rtp_service::{config::Settings, Service};
use std::{
    io,
    path::Path,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};
static SIGNAL: AtomicU8 = AtomicU8::new(0);
extern "C" fn signal(value: i32) {
    SIGNAL.store(if value == libc::SIGHUP { 1 } else { 2 }, Ordering::Relaxed);
}
fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err(io::Error::other(
            "usage: cpxy-rtp run|doctor|cleanup PROFILE.json",
        ));
    }
    let path = Path::new(&args[2]);
    let settings = Settings::load(path)?;
    match args[1].as_str() {
        "doctor" => println!("{}", rtp_service::doctor(&settings)),
        "cleanup" => rtp_service::cleanup(&settings)?,
        "run" => {
            unsafe {
                libc::signal(libc::SIGTERM, signal as *const () as libc::sighandler_t);
                libc::signal(libc::SIGINT, signal as *const () as libc::sighandler_t);
                libc::signal(libc::SIGHUP, signal as *const () as libc::sighandler_t);
            }
            let mut service = Service::new(settings, Arc::new(|event| eprintln!("{event}")))?;
            service.start()?;
            while service.is_running() {
                match SIGNAL.swap(0, Ordering::Relaxed) {
                    2 => break,
                    1 => match Settings::load(path).and_then(|s| service.reload(s)) {
                        Ok(()) => {}
                        Err(e) => eprintln!(
                            "{}",
                            serde_json::json!({"event":"reload_failed","error":e.to_string()})
                        ),
                    },
                    _ => {}
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            service.stop()?;
        }
        _ => return Err(io::Error::other("unknown command")),
    }
    Ok(())
}
