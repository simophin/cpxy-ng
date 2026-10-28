//! Bounded HTTP control service, application deduplication and supervised leases.
pub mod operations;
pub mod wire;

pub mod leases;

pub mod client;

pub mod server;

pub mod transport;

/// The runtime each profile's transport runs on: packet I/O and its pump on two workers,
/// blocking HTTP and admission work on the blocking pool.
fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("rtp-runtime")
        .enable_all()
        .build()
}

#[cfg(test)]
mod tests;
