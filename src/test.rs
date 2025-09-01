use crate::server::configure_tls_connector;
use crate::{client, server};
use anyhow::Context;
use chacha20poly1305::aead::OsRng;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
use dotenvy::dotenv;
use reqwest::Proxy;
use tokio::net::TcpListener;
use tokio::try_join;

#[tokio::test]
async fn test_server_client_works() {
    let _ = dotenv();
    let _ = tracing_subscriber::fmt::try_init();

    let key = ChaCha20Poly1305::generate_key(&mut OsRng);

    let server = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let run_server = async {
        let connector = configure_tls_connector();
        loop {
            let (conn, addr) = server.accept().await.context("Accept failed")?;
            server::handle_connection(conn, addr, key, connector.clone()).await?;
        }
        anyhow::Ok(())
    };

    let server_port = server.local_addr().unwrap().port();

    let proxy_server = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let run_proxy_server = async {
        loop {
            let (conn, addr) = proxy_server.accept().await.context("Accept failed")?;
            client::accept_proxy_connection(conn, "127.0.0.1".to_string(), server_port, key)
                .await?;
        }

        anyhow::Ok(())
    };

    let proxy_server_port = proxy_server.local_addr().unwrap().port();

    let fetch_through_proxy = async {
        reqwest::Client::builder()
            .proxy(
                Proxy::all(format!("http://127.0.0.1:{proxy_server_port}"))
                    .context("Error setting proxy")?,
            )
            .build()
            .context("Error building client")?
            .get("https://www.google.com")
            .send()
            .await
            .context("Error sending request")?
            .text()
            .await
            .context("Error getting text to text")
    };

    let (_, _, fetched) =
        try_join!(run_server, run_proxy_server, fetch_through_proxy).expect("run server failed");
    assert!(fetched.contains("Google"));
}
