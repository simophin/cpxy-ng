use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn signal(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}
fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 || !matches!(args[1].as_str(), "run" | "managed" | "cleanup") {
        return Err(io::Error::other(
            "usage: cpxy-udp-relay run|managed|cleanup CONFIG.json",
        ));
    }
    unsafe {
        libc::signal(libc::SIGTERM, signal as *const () as _);
        libc::signal(libc::SIGINT, signal as *const () as _);
    }
    if args[1] == "cleanup" {
        let config = serde_json::from_slice(&std::fs::read(&args[2])?)?;
        return udp_relay::management::cleanup(config);
    }
    if args[1] == "managed" {
        let config = serde_json::from_slice(&std::fs::read(&args[2])?)?;
        return udp_relay::management::run(config, &STOP);
    }
    let config = serde_json::from_slice(&std::fs::read(&args[2])?)?;
    let mut relay = udp_relay::Relay::bind(config)?;
    unsafe {
        libc::signal(libc::SIGTERM, signal as *const () as _);
        libc::signal(libc::SIGINT, signal as *const () as _);
    }
    eprintln!(
        "{}",
        serde_json::json!({"event":"ready","inventory":relay.inventory()})
    );
    let mut at = Instant::now();
    while !STOP.load(Ordering::Relaxed) {
        relay.tick();
        std::thread::sleep(Duration::from_millis(2));
        if at.elapsed() >= Duration::from_secs(10) {
            eprintln!("{}", relay.inventory());
            at = Instant::now();
        }
    }
    eprintln!(
        "{}",
        serde_json::json!({"event":"stopped","inventory":relay.inventory()})
    );
    Ok(())
}
