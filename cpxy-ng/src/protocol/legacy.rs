//! The original request and response, from before [`ClientMessage`](super::ClientMessage).
//!
//! Frozen: clients and servers that predate the current messages still send these, so their
//! archived layout must not change. New code sends the current messages; the server decodes these
//! only so older clients keep working, and the client still sends them until every server it may
//! talk to understands the current ones. Delete this module once nothing sends them.

use crate::encrypt_stream::Configuration;
use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct Request {
    pub host: String,
    pub port: u16,
    /// Asked the server to open TLS to the target, which it no longer does: servers refuse
    /// `true`.
    pub tls: bool,
    pub client_send_cipher: Configuration,
    pub server_send_cipher: Configuration,
    pub initial_plaintext: Vec<u8>,
    /// Unused: clients send 0.
    pub timestamp_epoch_seconds: u64,
}

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub enum Response {
    Success {
        /// Bytes the target sent first, from servers that waited for them before confirming.
        /// Current servers confirm straight away and leave this empty.
        initial_response: Vec<u8>,
        timestamp_epoch_seconds: u64,
    },

    Error {
        msg: String,
        timestamp_epoch_seconds: u64,
    },
}
