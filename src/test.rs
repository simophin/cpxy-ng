use crate::http_stream::HttpStream;
use crate::server::configure_tls_connector;
use crate::{client, server};
use anyhow::Context;
use chacha20poly1305::aead::OsRng;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
use dotenvy::dotenv;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

async fn setup_proxy() -> u16 {
    let key = ChaCha20Poly1305::generate_key(&mut OsRng);

    let server = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let server_port = server.local_addr().unwrap().port();

    let run_server = async move {
        let connector = configure_tls_connector();
        loop {
            let (conn, addr) = server.accept().await.context("Accept failed")?;
            server::handle_connection(conn, addr, key, connector.clone()).await?;
        }
        anyhow::Ok(())
    };

    tokio::spawn(run_server);

    let proxy_server = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_server_port = proxy_server.local_addr().unwrap().port();

    let run_proxy_server = async move {
        loop {
            let (conn, addr) = proxy_server.accept().await.context("Accept failed")?;
            client::accept_proxy_connection(conn, "127.0.0.1".to_string(), server_port, key)
                .await?;
        }

        anyhow::Ok(())
    };

    tokio::spawn(run_proxy_server);
    proxy_server_port
}

#[tokio::test]
async fn test_http_proxy_works() {
    let _ = dotenv();
    let _ = tracing_subscriber::fmt::try_init();

    let proxy_server_port = setup_proxy().await;

    let fetched = async {
        let mut c = TcpStream::connect(("127.0.0.1", proxy_server_port))
            .await
            .context("Error opening connection to proxy server")?;

        c.write_all(b"GET http://www.google.com/ HTTP/1.1\r\nHost: www.google.com\r\nConnection: close\r\n\r\n")
            .await
            .context("Error writing to proxy server")?;

        let stream = HttpStream::parse_response(c, |resp| {
            assert_eq!(resp.code, Some(200));
            anyhow::Ok(())
        })
        .await?;

        let mut buf = String::new();
        BufReader::new(stream)
            .read_to_string(&mut buf)
            .await
            .context("to read")?;

        anyhow::Ok(buf)
    }.await.expect("To fetch");

    assert!(fetched.contains("Google"));
}

#[tokio::test]
async fn test_http_tunnel_works() {
    let _ = dotenv();
    let _ = tracing_subscriber::fmt::try_init();

    let proxy_server_port = setup_proxy().await;

    async {
        let mut c = TcpStream::connect(("127.0.0.1", proxy_server_port))
            .await
            .context("Error opening connection to proxy server")?;

        c.write_all(b"CONNECT www.google.com:80 HTTP/1.1\r\n\r\n")
            .await
            .context("Error writing to proxy server")?;

        c.write_all(
            b"GET /not_found HTTP/1.1\r\nHost: www.google.com\r\nConnection: close\r\n\r\n",
        )
        .await
        .context("Error writing to proxy server")?;

        let stream = HttpStream::parse_response(c, |resp| {
            assert_eq!(resp.code, Some(200));
            anyhow::Ok(())
        })
        .await?;

        let stream = HttpStream::parse_response(stream, |resp| {
            assert_eq!(resp.code, Some(404));
            anyhow::Ok(())
        })
        .await?;

        let mut buf = String::new();
        BufReader::new(stream)
            .read_to_string(&mut buf)
            .await
            .context("to read")?;

        anyhow::Ok(buf)
    }
    .await
    .expect("To fetch");
}
