//! Strict, bounded HTTP/1.1 framing. Paths never select an RPC operation.
use crate::v2::http::{MAX_BODY, random_path};
use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
const MAX_HEADERS: usize = 8192;
const TIMEOUT: Duration = Duration::from_secs(5);
const NOT_FOUND: &[u8] =
    b"HTTP/1.1 404 Not Found\r\nContent-Length: 10\r\nConnection: close\r\n\r\nNot Found\n";
fn bad() -> io::Error {
    io::Error::other("invalid control HTTP framing")
}
fn read_more(
    stream: &mut TcpStream,
    buffer: &mut Vec<u8>,
    limit: usize,
    deadline: Instant,
) -> io::Result<()> {
    stream.set_read_timeout(Some(
        deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(bad)?,
    ))?;
    let mut chunk = [0; 1024];
    let cap = chunk.len().min(limit.saturating_sub(buffer.len()));
    if cap == 0 {
        return Err(bad());
    }
    let n = stream.read(&mut chunk[..cap])?;
    if n == 0 {
        return Err(bad());
    }
    buffer.extend_from_slice(&chunk[..n]);
    Ok(())
}
fn target(path: &str) -> bool {
    if !path.starts_with('/') || path.len() > 2048 {
        return false;
    }
    let b = path.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 2 >= b.len() || !b[i + 1].is_ascii_hexdigit() || !b[i + 2].is_ascii_hexdigit() {
                return false;
            }
            i += 3;
            continue;
        }
        if !b[i].is_ascii_alphanumeric() && !b"-._~!$&'()*+,;=:@/?".contains(&b[i]) {
            return false;
        }
        i += 1;
    }
    true
}
fn length(headers: &[httparse::Header<'_>]) -> io::Result<usize> {
    let mut length = None;
    let mut host = 0;
    for h in headers {
        if h.name.eq_ignore_ascii_case("transfer-encoding")
            || h.name.eq_ignore_ascii_case("content-encoding")
        {
            return Err(bad());
        }
        if h.name.eq_ignore_ascii_case("host") {
            host += 1;
        }
        if h.name.eq_ignore_ascii_case("content-length") {
            if length.is_some() || h.value.is_empty() || !h.value.iter().all(u8::is_ascii_digit) {
                return Err(bad());
            }
            length = Some(
                std::str::from_utf8(h.value)
                    .map_err(|_| bad())?
                    .parse::<usize>()
                    .map_err(|_| bad())?,
            );
        }
    }
    if host > 1 {
        return Err(bad());
    }
    length.filter(|n| *n <= MAX_BODY).ok_or_else(bad)
}
fn request(stream: &mut TcpStream, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut buffer = Vec::new();
    let (head, size) = loop {
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut req = httparse::Request::new(&mut headers);
        match req.parse(&buffer).map_err(|_| bad())? {
            httparse::Status::Complete(n) => {
                if n > MAX_HEADERS
                    || req.method != Some("POST")
                    || req.version != Some(1)
                    || !req.path.is_some_and(target)
                    || req
                        .headers
                        .iter()
                        .filter(|h| h.name.eq_ignore_ascii_case("host"))
                        .count()
                        != 1
                    || req
                        .headers
                        .iter()
                        .any(|h| h.name.eq_ignore_ascii_case("host") && h.value.is_empty())
                {
                    return Err(bad());
                }
                break (n, length(req.headers)?);
            }
            httparse::Status::Partial => read_more(stream, &mut buffer, MAX_HEADERS, deadline)?,
        }
    };
    while buffer.len() < head + size {
        read_more(stream, &mut buffer, head + size, deadline)?;
    }
    if buffer.len() != head + size {
        return Err(bad());
    }
    Ok(buffer.split_off(head))
}
pub fn serve_one(
    mut stream: TcpStream,
    handler: impl FnOnce(Vec<u8>) -> Option<Vec<u8>>,
) -> io::Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let body = match request(&mut stream, deadline) {
        Ok(b) => handler(b),
        Err(_) => None,
    };
    if let Some(body) = body.filter(|b| b.len() <= MAX_BODY) {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        stream.write_all(&body)
    } else {
        stream.write_all(NOT_FOUND)
    }
}
pub fn post(mut stream: TcpStream, authority: &str, body: &[u8]) -> io::Result<Vec<u8>> {
    if body.len() > MAX_BODY || authority.bytes().any(|b| !b.is_ascii_graphic()) {
        return Err(bad());
    }
    stream.set_write_timeout(Some(TIMEOUT))?;
    write!(
        stream,
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        random_path(),
        authority,
        body.len()
    )?;
    stream.write_all(body)?;
    let deadline = Instant::now() + TIMEOUT;
    let mut buffer = Vec::new();
    let (head, size) = loop {
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut res = httparse::Response::new(&mut headers);
        match res.parse(&buffer).map_err(|_| bad())? {
            httparse::Status::Complete(n) => {
                if n > MAX_HEADERS || res.version != Some(1) {
                    return Err(bad());
                }
                if res.code == Some(404) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "control request rejected",
                    ));
                }
                if res.code != Some(200) {
                    return Err(bad());
                }
                break (n, length(res.headers)?);
            }
            httparse::Status::Partial => {
                read_more(&mut stream, &mut buffer, MAX_HEADERS, deadline)?
            }
        }
    };
    while buffer.len() < head + size {
        read_more(&mut stream, &mut buffer, head + size, deadline)?;
    }
    if buffer.len() != head + size {
        return Err(bad());
    }
    Ok(buffer.split_off(head))
}
pub struct Request {
    pub body: Vec<u8>,
    pub response: mpsc::SyncSender<Option<Vec<u8>>>,
    pub deadline: Instant,
}
/// All connection threads are bounded and joined before returning. A full queue
/// rejects new work without evicting an accepted command/replay cache record.
pub fn listen(
    listener: TcpListener,
    commands: mpsc::SyncSender<Request>,
    running: Arc<AtomicBool>,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let count = Arc::new(AtomicUsize::new(0));
    let mut threads: Vec<thread::JoinHandle<()>> = Vec::new();
    while running.load(Ordering::Relaxed) {
        let mut index = 0;
        while index < threads.len() {
            if threads[index].is_finished() {
                let _ = threads.swap_remove(index).join();
            } else {
                index += 1;
            }
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if count
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                        (n < 64).then_some(n + 1)
                    })
                    .is_err()
                {
                    continue;
                }
                let count = count.clone();
                let tx = commands.clone();
                let task = thread::Builder::new()
                    .name("rtp-http-connection".into())
                    .spawn(move || {
                        let _ = serve_one(stream, |body| {
                            let (response, rx) = mpsc::sync_channel(1);
                            tx.try_send(Request {
                                body,
                                response,
                                deadline: Instant::now() + TIMEOUT,
                            })
                            .ok()?;
                            rx.recv_timeout(TIMEOUT).ok().flatten()
                        });
                        count.fetch_sub(1, Ordering::AcqRel);
                    });
                match task {
                    Ok(t) => threads.push(t),
                    Err(e) => {
                        for t in threads {
                            let _ = t.join();
                        }
                        return Err(e);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(e) => return Err(e),
        }
    }
    drop(commands);
    for task in threads {
        let _ = task.join();
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::http::{Bootstrap, Codec, Kind, Server};
    fn roundtrip(raw: Vec<u8>, server: Arc<std::sync::Mutex<Server>>) -> Vec<u8> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let t = thread::spawn(move || {
            serve_one(listener.accept().unwrap().0, |b| {
                server.lock().unwrap().handle(&b, 0, |_| panic!())
            })
            .unwrap()
        });
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(&raw).unwrap();
        s.shutdown(std::net::Shutdown::Write).unwrap();
        let mut output = Vec::new();
        s.read_to_end(&mut output).unwrap();
        t.join().unwrap();
        output
    }
    #[test]
    fn unrelated_short_paths_dispatch_by_content_and_all_failures_match() {
        let server = Arc::new(std::sync::Mutex::new(
            Server::new(&[7; 32], "test", 16).unwrap(),
        ));
        let c = Codec::new(&[7; 32], "test").unwrap();
        let b = c
            .seal(
                Kind::Bootstrap,
                &Bootstrap {
                    token: "01".repeat(16),
                },
            )
            .unwrap();
        let mut good = None;
        for path in ["/a", "/unrelated/x", "/", "/query?x=1"] {
            let raw = [
                format!(
                    "POST {path} HTTP/1.1\r\nHost: example.invalid\r\nContent-Length: {}\r\n\r\n",
                    b.len()
                )
                .into_bytes(),
                b.clone(),
            ]
            .concat();
            let reply = roundtrip(raw, server.clone());
            assert!(reply.starts_with(b"HTTP/1.1 200"));
            if let Some(g) = &good {
                assert_eq!(&reply, g);
            } else {
                good = Some(reply);
            }
            for method in ["GET", "POST"] {
                let raw = format!("{method} {path} HTTP/1.1\r\nHost: example.invalid\r\nContent-Length: 3\r\n\r\nbad")
                    .into_bytes();
                assert_eq!(roundtrip(raw, server.clone()), NOT_FOUND);
            }
        }
        let mut tampered = b.clone();
        tampered[30] ^= 1;
        let raw = [
            format!(
                "POST /a HTTP/1.1\r\nHost: example.invalid\r\nContent-Length: {}\r\n\r\n",
                tampered.len()
            )
            .into_bytes(),
            tampered,
        ]
        .concat();
        assert_eq!(roundtrip(raw, server), NOT_FOUND);
    }
    #[test]
    fn ambiguous_framing_oversized_targets_and_transfer_encoding_rejected() {
        let server = Arc::new(std::sync::Mutex::new(
            Server::new(&[7; 32], "test", 1).unwrap(),
        ));
        for headers in [
            "Content-Length: 1\r\nContent-Length: 1",
            "Content-Length: 20000",
            "Content-Length: 1\r\nTransfer-Encoding: chunked",
            "Content-Length: -1",
            "Content-Length: 1\r\nContent-Encoding: gzip",
        ] {
            let raw = format!("POST /x HTTP/1.1\r\n{headers}\r\n\r\nx").into_bytes();
            assert_eq!(roundtrip(raw, server.clone()), NOT_FOUND);
        }
        for path in [
            format!("/{}", "a".repeat(2048)),
            "/bad%xx".into(),
            "/fragment#x".into(),
            "http://example/x".into(),
        ] {
            let raw = format!(
                "POST {path} HTTP/1.1\r\nHost: example.invalid\r\nContent-Length: 0\r\n\r\n"
            )
            .into_bytes();
            assert_eq!(roundtrip(raw, server.clone()), NOT_FOUND);
        }
    }
}
