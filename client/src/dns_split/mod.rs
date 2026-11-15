//! A forwarding DNS server that races queries across two groups of servers and picks an
//! answer based on whether its addresses are in the local region (see [`crate::region`]).

pub mod cache;
pub mod cli;
pub mod policy;
pub mod routing;
pub mod search;
pub mod server;
pub mod spec;
pub mod upstream;
