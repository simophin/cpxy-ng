//! The C ABI `cpxy-tsgw` (the Go Tailscale exit node in `gateway/`) links against: the engine in
//! gateway mode ([`crate::start_gateway`]) on one end of a socketpair whose other end tsnet reads
//! and writes. `include/cpxy_engine.h` declares these functions.
//!
//! Thread safety: every function may be called from any thread that is not an engine thread.
//! [`cpxy_engine_dns_resolve`], [`cpxy_engine_is_running`] and [`cpxy_engine_traffic`] may run
//! concurrently with each other; [`cpxy_engine_stop`] and [`cpxy_engine_free`] must not run
//! concurrently with any other call on the same handle, and nothing may use a handle after
//! [`cpxy_engine_free`].

use crate::{EngineHandle, EventListener, OutboundEvent};
use std::ffi::{CStr, c_char, c_int};
use std::os::fd::{FromRawFd, OwnedFd};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::null_mut;
use std::sync::Arc;

/// The query is malformed, an argument is invalid, or resolving failed.
pub const CPXY_ENGINE_ERR_FAILED: i64 = -1;
/// The engine is stopped.
pub const CPXY_ENGINE_ERR_STOPPED: i64 = -2;
/// The response does not fit the output buffer.
pub const CPXY_ENGINE_ERR_TOO_SMALL: i64 = -3;

/// An engine started by [`cpxy_engine_start`].
pub struct CpxyEngine(EngineHandle);

/// Connections are logged by the outbounds themselves; the gateway has no other use for them.
struct Ignore;

impl EventListener for Ignore {
    fn on_outbound_event(&self, _: OutboundEvent) {}
}

/// Starts the engine in gateway mode on `fd`, a descriptor that moves one IP packet per read and
/// write (a `SOCK_SEQPACKET` or `SOCK_DGRAM` socket, or a TUN). `config_json` is the apps' engine
/// config (see `src/config.rs`). Blocks while the DNS servers are set up. Logs go to stderr.
///
/// The engine takes ownership of a non-negative `fd` whether or not it starts, and closes it when
/// stopped or on failure. Returns null on failure, with the reason in `error_buffer` when it is
/// non-null; the buffer always holds a NUL-terminated string afterwards.
///
/// # Safety
///
/// `config_json` must be a readable NUL-terminated string. A non-null `error_buffer` must point to
/// at least `error_buffer_capacity` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cpxy_engine_start(
    fd: c_int,
    config_json: *const c_char,
    error_buffer: *mut c_char,
    error_buffer_capacity: u32,
) -> *mut CpxyEngine {
    unsafe { write_error(error_buffer, error_buffer_capacity, "") };
    init_logging();

    let result = catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<EngineHandle> {
        anyhow::ensure!(fd >= 0, "Invalid descriptor {fd}");
        // SAFETY: the caller hands the descriptor over.
        let tun = unsafe { OwnedFd::from_raw_fd(fd) };
        anyhow::ensure!(!config_json.is_null(), "No config");
        // SAFETY: the caller passes a NUL-terminated string.
        let config = unsafe { CStr::from_ptr(config_json) }.to_str()?;
        crate::start_gateway(tun, config, Arc::new(Ignore))
    }));
    let error = match result {
        Ok(Ok(handle)) => return Box::into_raw(Box::new(CpxyEngine(handle))),
        Ok(Err(e)) => format!("{e:#}"),
        Err(_) => "Starting the engine panicked".to_owned(),
    };
    unsafe { write_error(error_buffer, error_buffer_capacity, &error) };
    null_mut()
}

/// Stops the engine and closes its descriptor; blocks for up to a few seconds. Calling it again,
/// or with null, does nothing. The handle stays valid until [`cpxy_engine_free`].
///
/// # Safety
///
/// `engine` must be null or a live handle from [`cpxy_engine_start`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cpxy_engine_stop(engine: *const CpxyEngine) {
    if let Some(engine) = unsafe { engine.as_ref() } {
        engine.0.stop();
    }
}

/// Stops the engine if it runs and frees the handle. Null does nothing.
///
/// # Safety
///
/// `engine` must be null or a live handle from [`cpxy_engine_start`], freed at most once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cpxy_engine_free(engine: *mut CpxyEngine) {
    if !engine.is_null() {
        // SAFETY: the handle came from Box::into_raw in cpxy_engine_start.
        drop(unsafe { Box::from_raw(engine) });
    }
}

/// False once the packet worker has ended, e.g. because the other end of the descriptor closed,
/// so a supervisor can exit and be restarted.
///
/// # Safety
///
/// `engine` must be null or a live handle from [`cpxy_engine_start`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cpxy_engine_is_running(engine: *const CpxyEngine) -> bool {
    unsafe { engine.as_ref() }.is_some_and(|engine| engine.0.is_running())
}

/// IP bytes from the device into the engine (`sent`) and back (`received`). Either pointer may be
/// null.
///
/// # Safety
///
/// `engine` must be null or a live handle from [`cpxy_engine_start`]; non-null `sent` and
/// `received` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cpxy_engine_traffic(
    engine: *const CpxyEngine,
    sent: *mut u64,
    received: *mut u64,
) {
    let stats = unsafe { engine.as_ref() }
        .map(|engine| engine.0.traffic())
        .unwrap_or_default();
    // SAFETY: the caller passes writable pointers or null.
    unsafe {
        if let Some(sent) = sent.as_mut() {
            *sent = stats.sent;
        }
        if let Some(received) = received.as_mut() {
            *received = stats.received;
        }
    }
}

/// Answers one wire-format DNS query as the engine answers intercepted DNS (split DNS, cache, ad
/// blocking), writing the wire-format response to `out`. Blocks until answered; the DNS servers
/// time out after a few seconds. Returns the response length, or one of the negative
/// `CPXY_ENGINE_ERR_*` codes. 65535 bytes always suffice.
///
/// # Safety
///
/// `engine` must be a live handle from [`cpxy_engine_start`], `query` must point to `query_len`
/// readable bytes and `out` to `out_capacity` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cpxy_engine_dns_resolve(
    engine: *const CpxyEngine,
    query: *const u8,
    query_len: usize,
    out: *mut u8,
    out_capacity: usize,
) -> i64 {
    let Some(engine) = (unsafe { engine.as_ref() }) else {
        return CPXY_ENGINE_ERR_FAILED;
    };
    if query.is_null() || out.is_null() {
        return CPXY_ENGINE_ERR_FAILED;
    }
    if !engine.0.is_running() {
        return CPXY_ENGINE_ERR_STOPPED;
    }
    // SAFETY: the caller passes a readable buffer of this length.
    let query = unsafe { std::slice::from_raw_parts(query, query_len) };
    let response = match catch_unwind(AssertUnwindSafe(|| engine.0.resolve_dns(query))) {
        Ok(Ok(response)) => response,
        Ok(Err(e)) => {
            tracing::debug!("Error answering DNS over the peer API: {e:#}");
            return CPXY_ENGINE_ERR_FAILED;
        }
        Err(_) => return CPXY_ENGINE_ERR_FAILED,
    };
    if response.len() > out_capacity {
        return CPXY_ENGINE_ERR_TOO_SMALL;
    }
    // SAFETY: the caller passes a writable buffer of out_capacity bytes.
    unsafe { std::ptr::copy_nonoverlapping(response.as_ptr(), out, response.len()) };
    response.len() as i64
}

/// Copies `message` into the caller's buffer, truncated to fit with its NUL.
unsafe fn write_error(buffer: *mut c_char, capacity: u32, message: &str) {
    if buffer.is_null() || capacity == 0 {
        return;
    }
    let len = message.len().min(capacity as usize - 1);
    // SAFETY: the caller passes a writable buffer of `capacity` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(message.as_ptr(), buffer as *mut u8, len);
        *buffer.add(len) = 0;
    }
}

/// Logs to stderr, which systemd sends to the journal, without colours.
fn init_logging() {
    // Fails when already set by an earlier start, which is fine.
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use etherparse::{NetSlice, PacketBuilder, SlicedPacket, TransportSlice};
    use hickory_proto::op::{Message, MessageType, Query};
    use hickory_proto::rr::rdata::A;
    use hickory_proto::rr::{Name, RData, Record, RecordType};
    use std::net::{Ipv4Addr, UdpSocket};
    use std::os::fd::IntoRawFd;
    use std::time::Duration;

    const PEER: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 7);
    const ANSWER: Ipv4Addr = Ipv4Addr::new(93, 184, 216, 34);

    /// A DNS server that answers every A query with [`ANSWER`].
    fn fake_upstream() -> u16 {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok((n, from)) = socket.recv_from(&mut buf) {
                let Ok(request) = Message::from_vec(&buf[..n]) else {
                    continue;
                };
                let mut response = Message::new();
                response
                    .set_id(request.id())
                    .set_message_type(MessageType::Response)
                    .set_recursion_available(true)
                    .add_queries(request.queries().to_vec());
                for query in request.queries() {
                    response.add_answer(Record::from_rdata(
                        query.name().clone(),
                        300,
                        RData::A(A(ANSWER)),
                    ));
                }
                let _ = socket.send_to(&response.to_vec().unwrap(), from);
            }
        });
        port
    }

    fn query(id: u16) -> Message {
        let mut message = Message::new();
        message
            .set_id(id)
            .set_recursion_desired(true)
            .add_query(Query::query(
                Name::from_ascii("example.com.").unwrap(),
                RecordType::A,
            ));
        message
    }

    fn answers(response: &Message) -> Vec<Ipv4Addr> {
        response
            .answers()
            .iter()
            .filter_map(|r| match r.data() {
                RData::A(a) => Some(a.0),
                _ => None,
            })
            .collect()
    }

    fn start_engine(fd: c_int, config: &str) -> Result<*mut CpxyEngine, String> {
        let config = std::ffi::CString::new(config).unwrap();
        let mut error = [0 as c_char; 256];
        let engine =
            unsafe { cpxy_engine_start(fd, config.as_ptr(), error.as_mut_ptr(), error.len() as _) };
        if engine.is_null() {
            let message = unsafe { CStr::from_ptr(error.as_ptr()) };
            return Err(message.to_string_lossy().into_owned());
        }
        Ok(engine)
    }

    #[test]
    fn search_domains_reach_the_gateway_resolver() {
        let port = fake_upstream();
        let config = format!(
            r#"{{"server":"http://:k@127.0.0.1:1", "dns_upstream":["127.0.0.1:{port}"], "dns_alternative":["127.0.0.1:{port}"], "dns_search_domains":["home.arpa"]}}"#
        );
        let (_ours, theirs) = std::os::unix::net::UnixDatagram::pair().unwrap();
        let engine = start_engine(theirs.into_raw_fd(), &config).unwrap();
        let mut request = query(73);
        request.queries_mut()[0].set_name(Name::from_ascii("printer.").unwrap());
        let bytes = request.to_vec().unwrap();
        let mut out = [0u8; 4096];
        let n = unsafe {
            cpxy_engine_dns_resolve(
                engine,
                bytes.as_ptr(),
                bytes.len(),
                out.as_mut_ptr(),
                out.len(),
            )
        };
        unsafe {
            cpxy_engine_stop(engine);
            cpxy_engine_free(engine);
        }
        assert!(n > 0);
        let response = Message::from_vec(&out[..n as usize]).unwrap();
        assert_eq!(response.queries(), request.queries());
        assert_eq!(response.answers()[0].record_type(), RecordType::CNAME);
        assert_eq!(
            response.answers()[1].name().to_ascii(),
            "printer.home.arpa."
        );
        assert_eq!(answers(&response), [ANSWER]);
    }

    #[test]
    fn answers_dns_to_any_address_and_over_the_c_abi() {
        let port = fake_upstream();
        let config = format!(
            r#"{{"server": "http://:k@127.0.0.1:1", "dns_upstream": ["127.0.0.1:{port}"], "dns_alternative": ["127.0.0.1:{port}"], "mtu": 1280}}"#
        );
        let (ours, theirs) = std::os::unix::net::UnixDatagram::pair().unwrap();
        let engine = start_engine(theirs.into_raw_fd(), &config).unwrap();
        assert!(unsafe { cpxy_engine_is_running(engine) });

        // A UDP query to a public resolver, as an exit node's client would send it.
        let request = query(0x1234).to_vec().unwrap();
        let builder = PacketBuilder::ipv4(PEER.octets(), [8, 8, 8, 8], 64).udp(40000, 53);
        let mut packet = Vec::with_capacity(builder.size(request.len()));
        builder.write(&mut packet, &request).unwrap();
        ours.send(&packet).unwrap();

        ours.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut buf = [0u8; 2048];
        let n = ours.recv(&mut buf).unwrap();
        let sliced = SlicedPacket::from_ip(&buf[..n]).unwrap();
        let Some(NetSlice::Ipv4(ip)) = sliced.net else {
            panic!("not IPv4")
        };
        assert_eq!(ip.header().source_addr(), Ipv4Addr::new(8, 8, 8, 8));
        assert_eq!(ip.header().destination_addr(), PEER);
        let Some(TransportSlice::Udp(udp)) = sliced.transport else {
            panic!("not UDP")
        };
        assert_eq!((udp.source_port(), udp.destination_port()), (53, 40000));
        let response = Message::from_vec(udp.payload()).unwrap();
        assert_eq!(response.id(), 0x1234);
        assert_eq!(answers(&response), [ANSWER]);

        // The same resolver, as the peer API's DNS over HTTPS reaches it.
        let request = query(0).to_vec().unwrap();
        let mut out = [0u8; 65535];
        let n = unsafe {
            cpxy_engine_dns_resolve(
                engine,
                request.as_ptr(),
                request.len(),
                out.as_mut_ptr(),
                out.len(),
            )
        };
        assert!(n > 0, "resolve failed: {n}");
        let response = Message::from_vec(&out[..n as usize]).unwrap();
        assert_eq!(answers(&response), [ANSWER]);

        let n = unsafe {
            cpxy_engine_dns_resolve(engine, request.as_ptr(), request.len(), out.as_mut_ptr(), 4)
        };
        assert_eq!(n, CPXY_ENGINE_ERR_TOO_SMALL);
        let n = unsafe {
            cpxy_engine_dns_resolve(engine, [0u8; 3].as_ptr(), 3, out.as_mut_ptr(), out.len())
        };
        assert_eq!(n, CPXY_ENGINE_ERR_FAILED);

        let (mut sent, mut received) = (0, 0);
        unsafe { cpxy_engine_traffic(engine, &mut sent, &mut received) };
        assert_eq!(sent, packet.len() as u64);
        assert!(received > 0);

        unsafe { cpxy_engine_stop(engine) };
        assert!(!unsafe { cpxy_engine_is_running(engine) });
        let n = unsafe {
            cpxy_engine_dns_resolve(
                engine,
                request.as_ptr(),
                request.len(),
                out.as_mut_ptr(),
                out.len(),
            )
        };
        assert_eq!(n, CPXY_ENGINE_ERR_STOPPED);
        unsafe { cpxy_engine_free(engine) };
    }

    #[test]
    fn reports_start_errors_and_closes_the_descriptor() {
        let (ours, theirs) = std::os::unix::net::UnixDatagram::pair().unwrap();
        let error = start_engine(theirs.into_raw_fd(), "{}").unwrap_err();
        assert!(error.contains("Invalid engine config"), "{error}");
        // Sending fails once the other end is closed.
        assert!(ours.send(b"x").is_err());

        assert!(
            start_engine(-1, "{}")
                .unwrap_err()
                .contains("Invalid descriptor")
        );
        unsafe { cpxy_engine_free(null_mut()) };
    }
}
