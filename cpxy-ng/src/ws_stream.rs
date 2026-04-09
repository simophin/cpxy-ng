use rand::random;
use std::io::Error;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// WebSocket frame parser state machine.
enum FrameParser {
    /// Reading the mandatory 2-byte frame header.
    Header { bytes: [u8; 2], filled: usize },
    /// Reading a 2-byte (u16) extended payload length.
    ExtLen16 { has_mask: bool, bytes: [u8; 2], filled: usize },
    /// Reading an 8-byte (u64) extended payload length.
    ExtLen64 { has_mask: bool, bytes: [u8; 8], filled: usize },
    /// Reading the 4-byte masking key.
    MaskKey { payload_len: usize, bytes: [u8; 4], filled: usize },
    /// Reading (and optionally unmasking) payload bytes into the read buffer.
    Payload {
        remaining: usize,
        /// (masking_key, running_byte_offset) — None when the frame is unmasked.
        masking: Option<([u8; 4], usize)>,
    },
}

/// Wraps any `AsyncRead + AsyncWrite` stream and applies standard WebSocket binary framing.
///
/// * **Client side** (`is_client = true`): outgoing frames are masked (MASK bit set); incoming
///   frames are assumed unmasked (server→client direction per RFC 6455).
/// * **Server side** (`is_client = false`): outgoing frames are unmasked; incoming frames are
///   expected to carry a masking key (client→server direction).
///
/// Each `poll_write` call produces exactly one binary WebSocket frame.  Each `poll_read` call
/// strips frame headers, applies unmasking when required, and delivers the raw payload.
pub struct WsStream<S> {
    inner: S,
    is_client: bool,

    // Write path — assembled WS frame (header + optional masking key + payload).
    write_buf: Vec<u8>,
    write_pos: usize,

    // Read path — decoded payload bytes ready to hand to the caller.
    read_buf: Vec<u8>,
    read_pos: usize,
    // Frame-header parse state.
    parser: FrameParser,
}

impl<S> WsStream<S> {
    pub fn new(inner: S, is_client: bool) -> Self {
        Self {
            inner,
            is_client,
            write_buf: Vec::new(),
            write_pos: 0,
            read_buf: Vec::new(),
            read_pos: 0,
            parser: FrameParser::Header { bytes: [0; 2], filled: 0 },
        }
    }

    /// Encode `payload` as a single WS binary frame into `out`.
    fn build_frame(is_client: bool, payload: &[u8], out: &mut Vec<u8>) {
        out.clear();
        let len = payload.len();

        // Byte 0: FIN=1, RSV1-3=0, opcode=0x2 (binary).
        out.push(0x82);

        let mask_bit: u8 = if is_client { 0x80 } else { 0x00 };
        if len <= 125 {
            out.push(mask_bit | len as u8);
        } else if len <= 65535 {
            out.push(mask_bit | 126);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            out.push(mask_bit | 127);
            out.extend_from_slice(&(len as u64).to_be_bytes());
        }

        if is_client {
            let key: [u8; 4] = random();
            out.extend_from_slice(&key);
            for (i, &b) in payload.iter().enumerate() {
                out.push(b ^ key[i % 4]);
            }
        } else {
            out.extend_from_slice(payload);
        }
    }
}

// ── AsyncRead ─────────────────────────────────────────────────────────────────

impl<S: AsyncRead + Unpin> AsyncRead for WsStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            // 1. Serve decoded payload bytes if we have any ready.
            if self.read_pos < self.read_buf.len() {
                let n = (self.read_buf.len() - self.read_pos).min(out.remaining());
                out.put_slice(&self.read_buf[self.read_pos..self.read_pos + n]);
                self.read_pos += n;
                if self.read_pos == self.read_buf.len() {
                    self.read_buf.clear();
                    self.read_pos = 0;
                }
                return Poll::Ready(Ok(()));
            }

            // 2. Split fields so the borrow checker sees them as independent borrows.
            //    This is safe because WsStream<S>: Unpin when S: Unpin.
            let me = &mut *self;
            let inner = &mut me.inner;
            let read_buf = &mut me.read_buf;

            let next_parser: Option<FrameParser> = match &mut me.parser {
                // ── First 2 mandatory header bytes ────────────────────────────
                FrameParser::Header { bytes, filled } => {
                    let mut tmp = ReadBuf::new(&mut bytes[*filled..]);
                    ready!(Pin::new(&mut *inner).poll_read(cx, &mut tmp))?;
                    let n = tmp.filled().len();
                    if n == 0 {
                        return Poll::Ready(Ok(())); // EOF
                    }
                    *filled += n;
                    if *filled < 2 {
                        continue; // need the second byte
                    }

                    let has_mask = bytes[1] & 0x80 != 0;
                    let len7 = (bytes[1] & 0x7f) as usize;

                    match len7 {
                        126 => Some(FrameParser::ExtLen16 { has_mask, bytes: [0; 2], filled: 0 }),
                        127 => Some(FrameParser::ExtLen64 { has_mask, bytes: [0; 8], filled: 0 }),
                        _ if has_mask => Some(FrameParser::MaskKey {
                            payload_len: len7,
                            bytes: [0; 4],
                            filled: 0,
                        }),
                        _ => Some(FrameParser::Payload { remaining: len7, masking: None }),
                    }
                }

                // ── 2-byte extended length ────────────────────────────────────
                FrameParser::ExtLen16 { has_mask, bytes, filled } => {
                    let has_mask = *has_mask;
                    let mut tmp = ReadBuf::new(&mut bytes[*filled..]);
                    ready!(Pin::new(&mut *inner).poll_read(cx, &mut tmp))?;
                    let n = tmp.filled().len();
                    if n == 0 {
                        return Poll::Ready(Ok(()));
                    }
                    *filled += n;
                    if *filled < 2 {
                        continue;
                    }

                    let payload_len = u16::from_be_bytes(*bytes) as usize;
                    if has_mask {
                        Some(FrameParser::MaskKey { payload_len, bytes: [0; 4], filled: 0 })
                    } else {
                        Some(FrameParser::Payload { remaining: payload_len, masking: None })
                    }
                }

                // ── 8-byte extended length ────────────────────────────────────
                FrameParser::ExtLen64 { has_mask, bytes, filled } => {
                    let has_mask = *has_mask;
                    let mut tmp = ReadBuf::new(&mut bytes[*filled..]);
                    ready!(Pin::new(&mut *inner).poll_read(cx, &mut tmp))?;
                    let n = tmp.filled().len();
                    if n == 0 {
                        return Poll::Ready(Ok(()));
                    }
                    *filled += n;
                    if *filled < 8 {
                        continue;
                    }

                    let payload_len = u64::from_be_bytes(*bytes) as usize;
                    if has_mask {
                        Some(FrameParser::MaskKey { payload_len, bytes: [0; 4], filled: 0 })
                    } else {
                        Some(FrameParser::Payload { remaining: payload_len, masking: None })
                    }
                }

                // ── 4-byte masking key ────────────────────────────────────────
                FrameParser::MaskKey { payload_len, bytes, filled } => {
                    let payload_len = *payload_len;
                    let mut tmp = ReadBuf::new(&mut bytes[*filled..]);
                    ready!(Pin::new(&mut *inner).poll_read(cx, &mut tmp))?;
                    let n = tmp.filled().len();
                    if n == 0 {
                        return Poll::Ready(Ok(()));
                    }
                    *filled += n;
                    if *filled < 4 {
                        continue;
                    }

                    Some(FrameParser::Payload {
                        remaining: payload_len,
                        masking: Some((*bytes, 0)),
                    })
                }

                // ── Payload bytes ─────────────────────────────────────────────
                FrameParser::Payload { remaining, masking } => {
                    if *remaining == 0 {
                        // Frame complete; begin the next one.
                        Some(FrameParser::Header { bytes: [0; 2], filled: 0 })
                    } else {
                        // Read up to 4 KiB at a time directly into read_buf.
                        let chunk = (*remaining).min(4096);
                        let start = read_buf.len();
                        read_buf.resize(start + chunk, 0);

                        let mut tmp = ReadBuf::new(&mut read_buf[start..]);
                        ready!(Pin::new(&mut *inner).poll_read(cx, &mut tmp))?;
                        let n = tmp.filled().len();
                        read_buf.truncate(start + n);

                        if n == 0 {
                            return Poll::Ready(Ok(())); // EOF
                        }

                        // Unmask in-place if required.
                        if let Some((key, offset)) = masking {
                            for i in 0..n {
                                read_buf[start + i] ^= key[(*offset + i) % 4];
                            }
                            *offset += n;
                        }
                        *remaining -= n;

                        // No parser transition; loop back so step 1 can serve the bytes.
                        None
                    }
                }
            };

            if let Some(state) = next_parser {
                me.parser = state;
            }
        }
    }
}

// ── AsyncWrite ────────────────────────────────────────────────────────────────

impl<S: AsyncWrite + Unpin> AsyncWrite for WsStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, Error>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        // Build the frame once; subsequent retries after Pending reuse it.
        if self.write_buf.is_empty() {
            let is_client = self.is_client;
            Self::build_frame(is_client, buf, &mut self.write_buf);
            self.write_pos = 0;
        }

        // Flush the assembled frame.  Split borrows so we can read write_buf while
        // mutably borrowing inner and write_pos.
        let me = &mut *self;
        let inner = &mut me.inner;
        let write_buf: &[u8] = &me.write_buf;
        let write_pos = &mut me.write_pos;

        while *write_pos < write_buf.len() {
            let n =
                ready!(Pin::new(&mut *inner).poll_write(cx, &write_buf[*write_pos..]))?;
            if n == 0 {
                return Poll::Ready(Err(Error::new(
                    std::io::ErrorKind::WriteZero,
                    "underlying stream accepted zero bytes",
                )));
            }
            *write_pos += n;
        }

        let payload_len = buf.len();
        me.write_buf.clear();
        me.write_pos = 0;
        Poll::Ready(Ok(payload_len))
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn roundtrip(data: &[u8]) {
        let (client_io, server_io) = tokio::io::duplex(65536);
        let mut client = WsStream::new(client_io, true);
        let mut server = WsStream::new(server_io, false);

        // client → server
        client.write_all(data).await.unwrap();
        let mut received = vec![0u8; data.len()];
        server.read_exact(&mut received).await.unwrap();
        assert_eq!(data, received.as_slice(), "client→server mismatch");

        // server → client
        server.write_all(data).await.unwrap();
        let mut received2 = vec![0u8; data.len()];
        client.read_exact(&mut received2).await.unwrap();
        assert_eq!(data, received2.as_slice(), "server→client mismatch");
    }

    #[tokio::test]
    async fn ws_framing_small() {
        roundtrip(b"hello websocket").await;
    }

    #[tokio::test]
    async fn ws_framing_126_threshold() {
        // Just below, at, and just above the 1-byte length threshold.
        roundtrip(&vec![0xABu8; 125]).await;
        roundtrip(&vec![0xCDu8; 126]).await;
        roundtrip(&vec![0xEFu8; 200]).await;
    }

    #[tokio::test]
    async fn ws_framing_large() {
        roundtrip(&vec![0x42u8; 65536]).await;
    }

    #[tokio::test]
    async fn ws_framing_multiple_writes() {
        let (client_io, server_io) = tokio::io::duplex(65536);
        let mut client = WsStream::new(client_io, true);
        let mut server = WsStream::new(server_io, false);

        let msg = b"hello world foo bar baz";
        for chunk in msg.chunks(3) {
            client.write_all(chunk).await.unwrap();
        }

        let mut buf = vec![0u8; msg.len()];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, msg);
    }
}
