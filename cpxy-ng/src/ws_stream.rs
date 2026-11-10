use async_tungstenite::WebSocketStream;
use async_tungstenite::tokio::TokioAdapter;
use async_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio::io::{AsyncRead, AsyncWrite};

pub async fn new_ws_stream<S: AsyncRead + AsyncWrite + Unpin + Send>(
    conn: S,
    is_client: bool,
) -> ws_stream_tungstenite::WsStream<TokioAdapter<S>> {
    let role = if is_client { Role::Client } else { Role::Server };
    let ws = WebSocketStream::from_raw_socket(TokioAdapter::new(conn), role, Some(config())).await;
    ws_stream_tungstenite::WsStream::new(ws)
}

/// Per-connection WebSocket buffers. tungstenite's defaults (128 KiB read and write buffers,
/// unbounded write buffer) cost several hundred KiB per tunnel, which adds up on a router.
///
/// The relays write at most a few KiB per call, and `WsStream` turns each write into one
/// message, so the write side flushes every message instead of batching. `WsStream` also
/// caps a single write at `max_write_buffer_size`, which must leave room for the frame header.
fn config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(256 * 1024)
        .max_message_size(Some(256 * 1024 - 64))
}
