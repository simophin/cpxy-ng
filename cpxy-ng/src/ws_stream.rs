use async_tungstenite::WebSocketStream;
use async_tungstenite::tokio::TokioAdapter;
use async_tungstenite::tungstenite::protocol::Role;
use tokio::io::{AsyncRead, AsyncWrite};

pub async fn new_ws_stream<S: AsyncRead + AsyncWrite + Unpin + Send>(
    conn: S,
    is_client: bool,
) -> ws_stream_tungstenite::WsStream<TokioAdapter<S>> {
    let role = if is_client { Role::Client } else { Role::Server };
    let ws = WebSocketStream::from_raw_socket(TokioAdapter::new(conn), role, None).await;
    ws_stream_tungstenite::WsStream::new(ws)
}
