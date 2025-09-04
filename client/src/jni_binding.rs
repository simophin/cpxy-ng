use anyhow::Context;
use cpxy_ng::key_util::derive_password;
use jni::{
    JNIEnv, JavaVM,
    objects::{JClass, JObject, JString, JValueGen},
    sys::jlong,
};
use tokio::runtime::Runtime;
use tokio::{net::TcpListener, task::JoinHandle};

use crate::client;

struct ClientInstance {
    _runtime: Runtime,
    handle: JoinHandle<anyhow::Result<()>>,
}

fn get_java_vm() -> anyhow::Result<JavaVM> {
    let jvm = unsafe { JavaVM::from_raw(jni::sys::JNI_GetCreatedJavaVMs as *mut _) }?;
    Ok(jvm)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fanchao_cpxy_Client_create<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    server_host: JString<'local>,
    server_port: u16,
    key: JString<'local>,
    bind_addr: JString<'local>,
    use_websocket: bool,
    error_callback: JObject<'local>,
) -> jlong {
    let result = || -> anyhow::Result<ClientInstance> {
        let server_host: String = env.get_string(&server_host)?.into();
        let key: String = env.get_string(&key)?.into();
        let bind_addr: String = env.get_string(&bind_addr)?.into();

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

        let error_callback = env.new_weak_ref(error_callback)?;

        let run_task = async move {
            loop {
                let (client, addr) = listener
                    .accept()
                    .await
                    .context("Error accepting connection")?;
                tracing::info!("Accepted connection from {addr}");

                tokio::spawn(client::accept_proxy_connection(
                    client,
                    server_host.clone(),
                    server_port,
                    key,
                    use_websocket,
                ));
            }

            anyhow::Ok(())
        };

        let handle = runtime.spawn(async move {
            if let Err(e) = run_task.await {
                let jvm = get_java_vm()?;
                if let Some(error_callback) = error_callback {
                    let mut attach = jvm.attach_current_thread()?;
                    if let Some(callback) = error_callback.upgrade_local(&attach).ok().flatten() {
                        let error_text = attach.new_string(format!("{e}"))?;
                        let _ = attach.call_method(
                            callback,
                            "invoke",
                            "(Ljava/lang/String;)V",
                            &[JValueGen::Object(&error_text.into())],
                        );
                    }
                }
                Err(e)
            } else {
                Ok(())
            }
        });

        Ok(ClientInstance {
            _runtime: runtime,
            handle,
        })
    }();

    match result {
        Ok(instance) => Box::into_raw(Box::new(instance)) as jlong,
        Err(e) => {
            let _ = env.throw_new("java/lang/RuntimeException", format!("{e}"));
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fanchao_cpxy_Client_destroy<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    instance_ptr: jlong,
) {
    if instance_ptr == 0 {
        return;
    }

    let instance = unsafe { Box::from_raw(instance_ptr as *mut ClientInstance) };
    instance.handle.abort();
}
