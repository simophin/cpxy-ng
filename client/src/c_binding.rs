use anyhow::Context;
use cpxy_ng::key_util::derive_password;
use std::ffi::{CStr, CString, c_char, c_void};
use tokio::net::TcpListener;
use tokio::runtime::Runtime;

use crate::client;

struct ClientInstance {
    _runtime: Runtime,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn client_create(
    server_host: *const c_char,
    server_port: u16,
    key: *const c_char,
    bind_addr: *const c_char,
    error_message: *mut c_char,
    error_message_len: usize,
) -> *const c_void {
    let result = || -> anyhow::Result<Runtime> {
        let server_host: String = unsafe { CStr::from_ptr(server_host) }
            .to_str()
            .context("Invalid UTF-8 in server_host")?
            .to_string();
        let key: String = unsafe { CStr::from_ptr(key) }
            .to_str()
            .context("Invalid UTF-8 in key")?
            .to_string();
        let bind_addr: String = unsafe { CStr::from_ptr(bind_addr) }
            .to_str()
            .context("Invalid UTF-8 in bind_addr")?
            .to_string();

        let listener = std::net::TcpListener::bind(bind_addr.as_str())
            .with_context(|| format!("Error binding on address: {bind_addr}"))?;

        let key = derive_password(key.as_str()).into();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_io()
            .enable_time()
            .build()
            .context("Error building tokio runtime")?;

        let _guard = runtime.enter();

        let listener =
            TcpListener::from_std(listener).with_context(|| "Error creating async TcpListener")?;

        let run_task = async move {
            while let Ok((client, addr)) = listener.accept().await {
                tracing::info!("Accepted connection from {addr}");

                tokio::spawn(client::accept_proxy_connection(
                    client,
                    server_host.clone(),
                    server_port,
                    key,
                ));
            }
        };

        runtime.spawn(run_task);
        Ok(runtime)
    }();

    match result {
        Ok(runtime) => {
            let instance = Box::new(ClientInstance { _runtime: runtime });
            Box::into_raw(instance) as *const c_void
        }
        Err(e) => {
            if !error_message.is_null() {
                let message = CString::new(format!("{e:?}")).unwrap();
                let out = unsafe {
                    std::slice::from_raw_parts_mut(error_message as *mut u8, error_message_len)
                };
                let bytes = message.as_bytes_with_nul();
                let len = bytes.len().min(error_message_len);
                out[..len].copy_from_slice(&bytes[..len]);
            }

            std::ptr::null()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn client_destroy(instance: *const c_void) {
    unsafe {
        let _ = Box::from_raw(instance as *mut ClientInstance);
    }
}
