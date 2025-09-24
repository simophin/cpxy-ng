use anyhow::Context;
use cpxy_ng::dns_model;
use cpxy_ng::dns_model::{DnsModel, Request};
use hickory_resolver::Resolver;
use hickory_resolver::config::{NameServerConfig, ResolverConfig};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::proto::xfer::Protocol;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::task::JoinSet;

pub async fn serve(
    socket: UdpSocket,
    key: [u8; 32],
    cn_dns: &[Ipv4Addr],
    global_dns: &[Ipv4Addr],
) -> anyhow::Result<()> {
    let cn_resolver = Arc::new(
        Resolver::builder_with_config(build_resolver(&cn_dns), TokioConnectionProvider::default())
            .build(),
    );

    let global_resolver = Arc::new(
        Resolver::builder_with_config(
            build_resolver(&global_dns),
            TokioConnectionProvider::default(),
        )
        .build(),
    );

    let socket = Arc::new(socket);
    let mut js = JoinSet::new();
    let key = key.into();

    let mut buf = vec![0u8; 65536];
    while let Ok((size, addr)) = socket.recv_from(&mut buf).await {
        tracing::info!("Received {size} bytes from {addr}");
        let r = match dns_model::Request::decrypt(&key, &buf[..size]) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(?e, "Error decrypting request from {addr}");
                continue;
            }
        };

        let cn_resolver = cn_resolver.clone();
        let global_resolver = global_resolver.clone();
        let socket = socket.clone();

        js.spawn(async move {
            match r {
                Request::DnsResolve(domains) => {
                    let resp = super::resolver::resolve_dns_request(
                        domains.as_slice(),
                        cn_resolver.as_ref(),
                        global_resolver.as_ref(),
                    )
                    .await
                    .unwrap_or_default();

                    let data = resp.encrypt(&key).context("Error encrypting response")?;
                    socket
                        .send_to(&data, &addr)
                        .await
                        .context("Error sending response")?;
                }
            }

            anyhow::Ok(())
        });
    }

    Ok(())
}

fn build_resolver(nameservers: &[Ipv4Addr]) -> ResolverConfig {
    let mut config = ResolverConfig::new();
    for addr in nameservers {
        config.add_name_server(NameServerConfig::new(
            SocketAddr::new(IpAddr::V4(*addr), 53),
            Protocol::Udp,
        ))
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_server_works() {
        let _ = tracing_subscriber::fmt::try_init();
        let socket = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let local_addr = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            socket.local_addr().unwrap().port(),
        );
        let key = [0u8; 32];

        let _serving = tokio::spawn(async move {
            serve(
                socket,
                key,
                &[Ipv4Addr::new(114, 114, 114, 114)],
                &[Ipv4Addr::new(1, 1, 1, 1)],
            )
            .await
            .unwrap();
        });

        let socket = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let request_bytes = Request::DnsResolve(vec!["google.com".to_string()])
            .encrypt(&key.into())
            .unwrap();

        socket
            .send_to(request_bytes.as_slice(), local_addr)
            .await
            .unwrap();

        let mut buf = vec![0u8; 65536];
        let (size, _) = socket.recv_from(&mut buf).await.unwrap();
        let response = dns_model::DnsResolveResult::decrypt(&key.into(), &buf[..size]).unwrap();

        let result = response.result.get("google.com");
        assert!(result.is_some());

        let result = result.unwrap();
        assert!(!result.addresses.is_empty());
    }
}
