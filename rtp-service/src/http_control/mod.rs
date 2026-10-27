//! Bounded HTTP control service, application deduplication and supervised leases.
pub mod operations;
pub mod wire;

pub mod leases;

pub mod client;

pub mod server;

pub mod transport;

#[cfg(test)]
mod tests;
