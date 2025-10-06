use crate::http_proxy_server::HttpProxyHandshaker;
use crate::outbound::cn::cn_outbound;
use crate::protocol_config::Config;
use crate::proxy_handlers::serve_listener;
use crate::socks_proxy_server::SocksProxyHandshaker;
use crate::stats_server::{StatsProvider, serve_stats};
use anyhow::Context;
use futures::future::join3;
use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr::null_mut;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::sync::broadcast;
use url::Url;

struct Handle {
    _rt: Runtime,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn create_client(
    http_proxy_port: u16,
    socks5_proxy_port: u16,
    api_proxy_port: u16,
    main_server_url: *const c_char,
    ai_server_url: *const c_char,
    tailscale_server_url: *const c_char,
    error: *mut c_char,
    error_len: usize,
) -> *mut c_void {
    let r = (move || {
        let main_server_config = parse_config_from_url(main_server_url)
            .context("failed to parse main server url")?
            .context("Main server url is required")?;

        let ai_server_config =
            parse_config_from_url(ai_server_url).context("failed to parse ai server url")?;

        let tailscale_server_config = parse_config_from_url(tailscale_server_url)
            .context("failed to parse tailscale server url")?;

        let http_listener = std::net::TcpListener::bind(("0.0.0.0", http_proxy_port))
            .with_context(|| format!("Failed to bind http proxy on {http_proxy_port}"))?;

        http_listener
            .set_nonblocking(true)
            .context("Failed to set http listener to non-blocking")?;

        let socks_listener = std::net::TcpListener::bind(("0.0.0.0", socks5_proxy_port))
            .with_context(|| format!("Failed to bind socks5 proxy on {socks5_proxy_port}"))?;

        socks_listener
            .set_nonblocking(true)
            .context("Failed to set socks5 listener to non-blocking")?;

        let api_listener = std::net::TcpListener::bind(("127.0.0.1", api_proxy_port))
            .with_context(|| format!("Failed to bind api proxy on {api_proxy_port}"))?;

        api_listener
            .set_nonblocking(true)
            .context("Failed to set api listener to non-blocking")?;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .context("Error creating tokio runtime")?;

        let _guard = rt.enter();
        let http_proxy_listener = TcpListener::from_std(http_listener)
            .context("Failed to create tokio TcpListener for http")?;

        let socks5_proxy_listener = TcpListener::from_std(socks_listener)
            .context("Failed to create tokio TcpListener for socks5")?;

        let api_proxy_listener = TcpListener::from_std(api_listener)
            .context("Failed to create tokio TcpListener for api")?;

        let (events_tx, events) = broadcast::channel(100);

        let outbound = Arc::new(cn_outbound(
            main_server_config,
            ai_server_config,
            tailscale_server_config,
            events_tx,
        ));

        let handle_http_proxy =
            serve_listener::<HttpProxyHandshaker<_>, _>(http_proxy_listener, outbound.clone());

        let handle_socks5_proxy =
            serve_listener::<SocksProxyHandshaker<_>, _>(socks5_proxy_listener, outbound);

        let handle_api_proxy = serve_stats(StatsProvider { events }, api_proxy_listener);

        rt.spawn(join3(
            handle_http_proxy,
            handle_socks5_proxy,
            handle_api_proxy,
        ));

        anyhow::Ok(Handle { _rt: rt })
    })();

    match r {
        Ok(v) => Box::into_raw(Box::new(v)) as *mut c_void,
        Err(e) => {
            if error != null_mut() && error_len > 0 {
                // Write the error message to the provided buffer
                let err_str = CString::new(format!("{e:?}")).unwrap();
                let error_out =
                    unsafe { std::slice::from_raw_parts_mut(error as *mut u8, error_len) };
                let bytes = err_str.as_bytes_with_nul();
                let len = bytes.len().min(error_len);
                error_out[..len].copy_from_slice(&bytes[..len]);
            }

            null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn destroy_client(handle: *mut c_void) {
    if handle.is_null() {
        return;
    }

    let _handle = unsafe { Box::from_raw(handle as *mut Handle) };
}

fn parse_config_from_url(url: *const c_char) -> anyhow::Result<Option<Config>> {
    if url.is_null() {
        return Ok(None);
    }

    let url: Url = unsafe { CStr::from_ptr(url) }
        .to_str()
        .context("url is not valid UTF-8")?
        .try_into()
        .context("url is not valid URL")?;

    url.try_into()
        .context("url is not valid protocol config")
        .map(Some)
}
