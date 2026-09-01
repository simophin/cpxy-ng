use crate::http_proxy_server::HttpProxyHandshaker;
use crate::outbound::cn::cn_outbound;
use crate::protocol_config::Config;
use crate::proxy_handlers::serve_listener;
use crate::socks_proxy_server::SocksProxyHandshaker;
use crate::stats_server::{StatsProvider, serve_stats};
use anyhow::Context;
use futures::future::join3;
use std::any::Any;
use std::ffi::{CStr, c_char, c_void};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener as StdTcpListener};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::null_mut;
use std::str::FromStr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::sync::broadcast;
use url::Url;

pub const CPXY_CLIENT_ABI_VERSION: u32 = 1;

#[allow(non_camel_case_types)]
pub type cpxy_client_handle = *mut c_void;

struct Handle {
    _rt: Runtime,
}

const EMBEDDED_LISTEN_ADDRESS: Ipv4Addr = Ipv4Addr::LOCALHOST;

enum CreateFailure {
    Error(anyhow::Error),
    Panic(String),
}

#[unsafe(no_mangle)]
pub extern "C" fn cpxy_client_abi_version() -> u32 {
    CPXY_CLIENT_ABI_VERSION
}

/// Creates a client session.
///
/// All non-null string pointers must refer to readable, NUL-terminated C strings. `dns_server`
/// and `main_server_url` are required; the remaining URL pointers are optional. When non-null,
/// `error_buffer` must point to writable storage of at least `error_buffer_capacity` bytes.
///
/// # Safety
///
/// The caller must uphold the pointer validity requirements above for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cpxy_client_create(
    http_proxy_port: u16,
    socks5_proxy_port: u16,
    api_proxy_port: u16,
    dns_server: *const c_char,
    main_server_url: *const c_char,
    ai_server_url: *const c_char,
    tailscale_server_url: *const c_char,
    error_buffer: *mut c_char,
    error_buffer_capacity: u32,
) -> cpxy_client_handle {
    // Leave callers with a valid empty C string on success as well as on any failure that occurs
    // before an error message can be produced.
    unsafe { write_error(error_buffer, error_buffer_capacity, "") };

    let result = contain_create_panic(|| unsafe {
        create_client(
            http_proxy_port,
            socks5_proxy_port,
            api_proxy_port,
            dns_server,
            main_server_url,
            ai_server_url,
            tailscale_server_url,
        )
    });

    match result {
        Ok(handle) => Box::into_raw(Box::new(handle)) as cpxy_client_handle,
        Err(CreateFailure::Error(error)) => {
            unsafe { write_error(error_buffer, error_buffer_capacity, &format!("{error:?}")) };
            null_mut()
        }
        Err(CreateFailure::Panic(message)) => {
            unsafe {
                write_error(
                    error_buffer,
                    error_buffer_capacity,
                    &format!("client creation panicked: {message}"),
                )
            };
            null_mut()
        }
    }
}

fn contain_create_panic(
    create: impl FnOnce() -> anyhow::Result<Handle>,
) -> Result<Handle, CreateFailure> {
    match catch_unwind(AssertUnwindSafe(create)) {
        Ok(result) => result.map_err(CreateFailure::Error),
        Err(payload) => Err(CreateFailure::Panic(panic_message(payload.as_ref()).into())),
    }
}

unsafe fn create_client(
    http_proxy_port: u16,
    socks5_proxy_port: u16,
    api_proxy_port: u16,
    dns_server: *const c_char,
    main_server_url: *const c_char,
    ai_server_url: *const c_char,
    tailscale_server_url: *const c_char,
) -> anyhow::Result<Handle> {
    let main_server_config = unsafe { parse_required_config(main_server_url, "main server url") }
        .context("failed to parse main server url")?;

    let dns_server = unsafe { required_c_str(dns_server, "dns server address") }?
        .to_str()
        .context("dns server address is not valid UTF-8")?
        .parse::<IpAddr>()
        .context("dns server address is not a valid IP address")?;

    let ai_server_config =
        unsafe { parse_optional_config(ai_server_url) }.context("failed to parse ai server url")?;

    let tailscale_server_config = unsafe { parse_optional_config(tailscale_server_url) }
        .context("failed to parse tailscale server url")?;

    let http_listener = bind_embedded_listener("http proxy", http_proxy_port)?;

    http_listener
        .set_nonblocking(true)
        .context("Failed to set http listener to non-blocking")?;

    let socks_listener = bind_embedded_listener("socks5 proxy", socks5_proxy_port)?;

    socks_listener
        .set_nonblocking(true)
        .context("Failed to set socks5 listener to non-blocking")?;

    let api_listener = bind_embedded_listener("api proxy", api_proxy_port)?;

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
        vec![SocketAddr::new(dns_server, 53)],
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

    Ok(Handle { _rt: rt })
}

fn bind_embedded_listener(name: &str, port: u16) -> anyhow::Result<StdTcpListener> {
    StdTcpListener::bind(embedded_listener_address(port))
        .with_context(|| format!("Failed to bind {name} on {EMBEDDED_LISTEN_ADDRESS}:{port}"))
}

fn embedded_listener_address(port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(EMBEDDED_LISTEN_ADDRESS, port)
}

/// Destroys a client session. A null handle is a no-op.
///
/// # Safety
///
/// A non-null handle must have been returned by `cpxy_client_create`, and it must be destroyed at
/// most once. The handle must not be used after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cpxy_client_destroy(handle: cpxy_client_handle) {
    if handle.is_null() {
        return;
    }

    // Runtime destruction is not expected to panic, but no Rust panic may cross the C boundary.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _handle = unsafe { Box::from_raw(handle as *mut Handle) };
    }));
}

unsafe fn required_c_str<'a>(value: *const c_char, name: &str) -> anyhow::Result<&'a CStr> {
    if value.is_null() {
        anyhow::bail!("{name} is required");
    }

    Ok(unsafe { CStr::from_ptr(value) })
}

unsafe fn parse_required_config(url: *const c_char, name: &str) -> anyhow::Result<Config> {
    let value = unsafe { required_c_str(url, name) }?
        .to_str()
        .with_context(|| format!("{name} is not valid UTF-8"))?;
    let url = Url::from_str(value).with_context(|| format!("{name} is not a valid URL"))?;

    url.try_into()
        .with_context(|| format!("{name} is not a valid protocol config"))
}

unsafe fn parse_optional_config(url: *const c_char) -> anyhow::Result<Option<Config>> {
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

unsafe fn write_error(buffer: *mut c_char, capacity: u32, message: &str) {
    if buffer.is_null() || capacity == 0 {
        return;
    }

    let capacity = capacity as usize;
    let copy_len = message.len().min(capacity - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(message.as_ptr(), buffer.cast::<u8>(), copy_len);
        buffer.cast::<u8>().add(copy_len).write(0);
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn embedded_listeners_bind_only_to_loopback() {
        assert_eq!(
            embedded_listener_address(8123),
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8123)
        );
    }

    fn call_create(
        dns: *const c_char,
        main: *const c_char,
        error: *mut c_char,
        capacity: u32,
    ) -> cpxy_client_handle {
        unsafe {
            cpxy_client_create(
                0,
                0,
                0,
                dns,
                main,
                std::ptr::null(),
                std::ptr::null(),
                error,
                capacity,
            )
        }
    }

    fn error_text(buffer: &[u8]) -> &str {
        CStr::from_bytes_until_nul(buffer)
            .unwrap()
            .to_str()
            .unwrap()
    }

    #[test]
    fn reports_declared_abi_version() {
        assert_eq!(cpxy_client_abi_version(), CPXY_CLIENT_ABI_VERSION);
        assert_eq!(CPXY_CLIENT_ABI_VERSION, 1);
    }

    #[test]
    fn rejects_null_required_pointers_without_panicking() {
        let mut error = [0x7f_u8; 128];
        let handle = call_create(
            std::ptr::null(),
            std::ptr::null(),
            error.as_mut_ptr().cast(),
            error.len() as u32,
        );

        assert!(handle.is_null());
        assert!(error_text(&error).contains("main server url is required"));
    }

    #[test]
    fn rejects_a_null_dns_pointer_without_panicking() {
        let main = CString::new("https://user:password@example.com").unwrap();
        let mut error = [0x7f_u8; 128];
        let handle = call_create(
            std::ptr::null(),
            main.as_ptr(),
            error.as_mut_ptr().cast(),
            error.len() as u32,
        );

        assert!(handle.is_null());
        assert!(error_text(&error).contains("dns server address is required"));
    }

    #[test]
    fn invalid_url_returns_a_terminated_error() {
        let dns = CString::new("1.1.1.1").unwrap();
        let main = CString::new("not a url").unwrap();
        let mut error = [0x7f_u8; 48];
        let handle = call_create(
            dns.as_ptr(),
            main.as_ptr(),
            error.as_mut_ptr().cast(),
            error.len() as u32,
        );

        assert!(handle.is_null());
        assert!(error_text(&error).contains("failed to parse main server url"));
        assert_eq!(error[error.len() - 1], 0);
    }

    #[test]
    fn invalid_dns_returns_a_terminated_error() {
        let dns = CString::new("not-an-ip").unwrap();
        let main = CString::new("https://user:password@example.com").unwrap();
        let mut error = [0x7f_u8; 256];
        let handle = call_create(
            dns.as_ptr(),
            main.as_ptr(),
            error.as_mut_ptr().cast(),
            error.len() as u32,
        );

        assert!(handle.is_null());
        assert!(error_text(&error).contains("not a valid IP address"));
        assert!(error.iter().position(|byte| *byte == 0).is_some());
    }

    #[test]
    fn truncated_errors_are_nul_terminated() {
        let mut error = [0x7f_u8; 8];
        unsafe {
            write_error(
                error.as_mut_ptr().cast(),
                error.len() as u32,
                "long error text",
            )
        };

        assert_eq!(&error, b"long er\0");
    }

    #[test]
    fn zero_and_one_byte_error_buffers_are_safe() {
        let handle = call_create(std::ptr::null(), std::ptr::null(), std::ptr::null_mut(), 0);
        assert!(handle.is_null());

        let mut byte = 0x7f_u8;
        let handle = call_create(
            std::ptr::null(),
            std::ptr::null(),
            (&mut byte as *mut u8).cast(),
            1,
        );
        assert!(handle.is_null());
        assert_eq!(byte, 0);
    }

    #[test]
    fn panic_payloads_are_contained_and_rendered() {
        let result = contain_create_panic(|| panic!("ffi test panic"));

        assert!(matches!(
            result,
            Err(CreateFailure::Panic(message)) if message == "ffi test panic"
        ));
    }

    #[test]
    fn destroy_null_is_safe() {
        unsafe { cpxy_client_destroy(std::ptr::null_mut()) };
    }
}
