//! Linux settings adapter for the shared production client runtime.
use super::client::Client;
use crate::config::{Role, Settings};
use rtp_l3::{
    v2::client_runtime::{self, ClientConfig},
    worker::{Events, PacketIo, SocketProvider},
};
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
};
pub fn run(
    settings: Settings,
    control: Client,
    packet: Box<dyn PacketIo>,
    provider: Arc<dyn SocketProvider>,
    events: Events,
    running: Arc<AtomicBool>,
) -> io::Result<()> {
    let Role::Client { entry, .. } = settings.transport else {
        return Err(io::Error::other("client settings required"));
    };
    client_runtime::run(
        ClientConfig {
            entry,
            inner: settings.inner()?.0,
            keepalive_seconds: settings.keepalive_seconds,
        },
        control,
        packet,
        provider,
        events,
        running,
    )
}
