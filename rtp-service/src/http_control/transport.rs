//! Linux settings adapter for the shared production client runtime.
use super::client::Client;
use crate::{
    config::{Role, Settings},
    network::WritePolicy,
};
use rtp_l3::{
    device,
    v2::client_runtime::{self, ClientConfig},
    worker::{Events, SocketProvider},
};
use std::{io, os::fd::OwnedFd, sync::Arc};
use tokio_util::sync::CancellationToken;

/// Carry `packet` (the TUN) through the tunnel until `stop` is cancelled or the worker fails.
/// `accept` filters packets before they are written to the TUN.
pub fn run(
    settings: Settings,
    control: Client,
    packet: OwnedFd,
    accept: WritePolicy,
    provider: Arc<dyn SocketProvider>,
    events: Events,
    stop: CancellationToken,
) -> io::Result<()> {
    let Role::Client { entry, .. } = settings.transport else {
        return Err(io::Error::other("client settings required"));
    };
    let config = ClientConfig {
        entry,
        inner: settings.inner()?.0,
        keepalive_seconds: settings.keepalive_seconds,
    };
    super::runtime()?.block_on(async move {
        let (device, port) = device::channel();
        let pump = tokio::spawn(device::pump(packet, port, accept));
        let result = client_runtime::run(config, control, device, provider, events, stop).await;
        // The worker dropped its channels, which ends the pump.
        let pumped = pump
            .await
            .map_err(|_| io::Error::other("packet pump panicked"))?;
        pumped.and(result)
    })
}
