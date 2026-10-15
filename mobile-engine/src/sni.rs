//! Finds the server name an app's TLS ClientHello asks for, as the bytes are relayed, so the
//! traffic log can show it in place of the address. The flow is never held up for it.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::sync::oneshot;

const RECORD_HEADER_LEN: usize = 5;
/// The largest TLS plaintext record (RFC 8446 section 5.1).
const MAX_RECORD_LEN: usize = 1 << 14;
const CONTENT_TYPE_HANDSHAKE: u8 = 0x16;
const HANDSHAKE_CLIENT_HELLO: u8 = 0x01;
const EXTENSION_SERVER_NAME: u16 = 0;
const NAME_TYPE_HOST_NAME: u8 = 0;

#[derive(Debug, PartialEq, Eq)]
pub enum Sniffed {
    /// The start of a ClientHello; more bytes are needed.
    Incomplete,
    /// A ClientHello naming this server.
    Name(String),
    /// Not a ClientHello, or one without a usable server name.
    None,
}

/// Reads the server name from the start of a TCP stream. Only a ClientHello in a single record is
/// understood, which is how clients send it.
pub fn sniff(data: &[u8]) -> Sniffed {
    let Some(header) = data.get(..RECORD_HEADER_LEN) else {
        return match data.first() {
            None | Some(&CONTENT_TYPE_HANDSHAKE) => Sniffed::Incomplete,
            Some(_) => Sniffed::None,
        };
    };
    let record_len = u16::from_be_bytes([header[3], header[4]]) as usize;
    if header[0] != CONTENT_TYPE_HANDSHAKE || header[1] != 3 || record_len > MAX_RECORD_LEN {
        return Sniffed::None;
    }
    match data.get(RECORD_HEADER_LEN..RECORD_HEADER_LEN + record_len) {
        None => Sniffed::Incomplete,
        Some(record) => server_name(record).map_or(Sniffed::None, Sniffed::Name),
    }
}

fn server_name(record: &[u8]) -> Option<String> {
    let mut r = Reader(record);
    if r.u8()? != HANDSHAKE_CLIENT_HELLO {
        return None;
    }
    let hello_len = r.u24()?;
    let mut hello = Reader(r.bytes(hello_len)?);
    // Legacy version and random.
    hello.bytes(2 + 32)?;
    let session_id_len = hello.u8()? as usize;
    hello.bytes(session_id_len)?;
    let cipher_suites_len = hello.u16()? as usize;
    hello.bytes(cipher_suites_len)?;
    let compression_methods_len = hello.u8()? as usize;
    hello.bytes(compression_methods_len)?;
    let extensions_len = hello.u16()? as usize;
    let mut extensions = Reader(hello.bytes(extensions_len)?);
    while !extensions.0.is_empty() {
        let kind = extensions.u16()?;
        let len = extensions.u16()? as usize;
        let data = extensions.bytes(len)?;
        if kind != EXTENSION_SERVER_NAME {
            continue;
        }
        let mut list = Reader(data);
        let list_len = list.u16()? as usize;
        let mut names = Reader(list.bytes(list_len)?);
        while !names.0.is_empty() {
            let name_type = names.u8()?;
            let len = names.u16()? as usize;
            let name = names.bytes(len)?;
            if name_type == NAME_TYPE_HOST_NAME {
                return host_name(name);
            }
        }
        return None;
    }
    None
}

/// The name, if it looks like a DNS name, lowercased and without a trailing dot.
fn host_name(name: &[u8]) -> Option<String> {
    let name = name.strip_suffix(b".").unwrap_or(name);
    let valid = !name.is_empty()
        && name.len() <= 253
        && name
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'));
    valid.then(|| String::from_utf8_lossy(name).to_ascii_lowercase())
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        if len > self.0.len() {
            return None;
        }
        let (head, rest) = self.0.split_at(len);
        self.0 = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.bytes(2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u24(&mut self) -> Option<usize> {
        let b = self.bytes(3)?;
        Some(u32::from_be_bytes([0, b[0], b[1], b[2]]) as usize)
    }
}

/// A stream that sniffs what is read from it for a server name, and sends it, or `None`, once it
/// knows. Reads pass through unchanged and are not delayed. When the stream fails or is dropped
/// first, the sender is dropped.
pub struct SniTap<S> {
    inner: S,
    sniffing: Option<(Vec<u8>, oneshot::Sender<Option<String>>)>,
}

impl<S> SniTap<S> {
    pub fn new(inner: S, name: oneshot::Sender<Option<String>>) -> Self {
        Self {
            inner,
            sniffing: Some((Vec::new(), name)),
        }
    }

    fn sniff(&mut self, read: &[u8]) {
        let Some((seen, _)) = &mut self.sniffing else {
            return;
        };
        let name = if read.is_empty() {
            // The end of the stream.
            None
        } else {
            seen.extend_from_slice(read);
            match sniff(seen) {
                Sniffed::Incomplete => return,
                Sniffed::Name(name) => Some(name),
                Sniffed::None => None,
            }
        };
        if let Some((_, tx)) = self.sniffing.take() {
            let _ = tx.send(name);
        }
    }
}

impl<S: AsyncRead + Unpin> SniTap<S> {
    /// Sniffs what the stream already has buffered, without waiting for more. For a stream about
    /// to be dropped, whose bytes would otherwise never be read.
    pub async fn sniff_buffered(&mut self) {
        use futures::FutureExt;
        let mut buf = [0u8; 4096];
        while self.sniffing.is_some() {
            match self.read(&mut buf).now_or_never() {
                Some(Ok(n)) if n > 0 => {}
                _ => return,
            }
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for SniTap<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        match &result {
            Poll::Ready(Ok(())) => self.sniff(&buf.filled()[before..]),
            Poll::Ready(Err(_)) => self.sniffing = None,
            Poll::Pending => {}
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for SniTap<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    /// A ClientHello record with the given server name extension data, after another extension.
    pub(crate) fn client_hello(server_name_ext: Option<&[u8]>) -> Vec<u8> {
        // supported_versions first, so the parser has to skip one.
        let mut extensions = vec![0x00, 0x2b, 0x00, 0x03, 0x02, 0x03, 0x04];
        if let Some(data) = server_name_ext {
            extensions.extend_from_slice(&0u16.to_be_bytes());
            extensions.extend_from_slice(&(data.len() as u16).to_be_bytes());
            extensions.extend_from_slice(data);
        }
        let mut hello = vec![0x03, 0x03];
        hello.extend_from_slice(&[7; 32]);
        hello.push(32);
        hello.extend_from_slice(&[9; 32]);
        hello.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]);
        hello.extend_from_slice(&[0x01, 0x00]);
        hello.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        hello.extend_from_slice(&extensions);

        let mut handshake = vec![HANDSHAKE_CLIENT_HELLO];
        handshake.extend_from_slice(&(hello.len() as u32).to_be_bytes()[1..]);
        handshake.extend_from_slice(&hello);
        let mut record = vec![CONTENT_TYPE_HANDSHAKE, 0x03, 0x01];
        record.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
        record.extend_from_slice(&handshake);
        record
    }

    pub(crate) fn server_name_ext(name: &[u8]) -> Vec<u8> {
        let mut entry = vec![NAME_TYPE_HOST_NAME];
        entry.extend_from_slice(&(name.len() as u16).to_be_bytes());
        entry.extend_from_slice(name);
        let mut list = (entry.len() as u16).to_be_bytes().to_vec();
        list.extend_from_slice(&entry);
        list
    }

    #[test]
    fn finds_the_server_name() {
        let hello = client_hello(Some(&server_name_ext(b"WWW.Example.com.")));
        assert_eq!(sniff(&hello), Sniffed::Name("www.example.com".into()));
    }

    #[test]
    fn needs_the_whole_record() {
        let hello = client_hello(Some(&server_name_ext(b"example.com")));
        for len in [0, 1, 4, 5, hello.len() - 1] {
            assert_eq!(sniff(&hello[..len]), Sniffed::Incomplete, "{len} bytes");
        }
    }

    #[test]
    fn gives_up_on_other_streams() {
        assert_eq!(sniff(b"GET / HTTP/1.1\r\n"), Sniffed::None);
        assert_eq!(sniff(b"SSH-2.0-OpenSSH_9.6\r\n"), Sniffed::None);
        // A handshake record too long to be real.
        assert_eq!(sniff(&[0x16, 0x03, 0x01, 0xff, 0xff]), Sniffed::None);
        assert_eq!(sniff(&client_hello(None)), Sniffed::None);
        let bad_name = server_name_ext(b"bad name");
        assert_eq!(sniff(&client_hello(Some(&bad_name))), Sniffed::None);
        // An extension that claims more than there is.
        let truncated = [0x00, 0x09, 0x00];
        assert_eq!(sniff(&client_hello(Some(&truncated))), Sniffed::None);
    }

    #[tokio::test]
    async fn taps_the_name_from_a_split_hello_without_changing_the_stream() {
        let hello = client_hello(Some(&server_name_ext(b"example.com")));
        let (mut app, engine) = tokio::io::duplex(64);
        let (tx, mut rx) = oneshot::channel();
        let mut tap = SniTap::new(engine, tx);

        let expected = [hello.as_slice(), b"rest"].concat();
        let writer = tokio::spawn({
            let expected = expected.clone();
            async move {
                for chunk in expected.chunks(10) {
                    app.write_all(chunk).await.unwrap();
                }
                app
            }
        });
        let mut read = vec![0u8; expected.len()];
        tap.read_exact(&mut read).await.unwrap();
        assert_eq!(read, expected);
        assert_eq!(rx.try_recv().unwrap().as_deref(), Some("example.com"));
        drop(writer.await.unwrap());
    }

    #[tokio::test]
    async fn sends_no_name_when_the_stream_ends_first() {
        let (app, engine) = tokio::io::duplex(64);
        let (tx, rx) = oneshot::channel();
        let mut tap = SniTap::new(engine, tx);
        drop(app);
        assert_eq!(tap.read(&mut [0u8; 16]).await.unwrap(), 0);
        assert_eq!(rx.await.unwrap(), None);
    }

    #[tokio::test]
    async fn sniffs_what_is_buffered() {
        let hello = client_hello(Some(&server_name_ext(b"example.com")));
        let (mut app, engine) = tokio::io::duplex(4096);
        app.write_all(&hello).await.unwrap();
        let (tx, rx) = oneshot::channel();
        let mut tap = SniTap::new(engine, tx);
        tap.sniff_buffered().await;
        assert_eq!(rx.await.unwrap().as_deref(), Some("example.com"));

        // With nothing buffered, it returns at once.
        let (_app, engine) = tokio::io::duplex(64);
        let (tx, _rx) = oneshot::channel();
        SniTap::new(engine, tx).sniff_buffered().await;
    }
}
