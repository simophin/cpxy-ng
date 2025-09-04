use anyhow::{Context, bail, ensure};
use bytes::{Buf, Bytes};
use std::io::Cursor;
use tokio::io::{AsyncRead, AsyncReadExt};

pub async fn parse_http_request<T>(
    stream: &mut (impl AsyncRead + Unpin),
    mut parser: impl FnMut(&httparse::Request<'_, '_>) -> anyhow::Result<T>,
) -> anyhow::Result<(T, Bytes)> {
    parse_http(stream, |data| {
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut http_req = httparse::Request::new(&mut headers);
        match http_req.parse(data).context("Parsing http request")? {
            httparse::Status::Complete(len) => {
                let result = parser(&http_req)?;
                Ok(Some((result, len)))
            }
            httparse::Status::Partial => Ok(None),
        }
    })
    .await
}

pub async fn parse_http_response<T>(
    stream: &mut (impl AsyncRead + Unpin),
    mut parser: impl FnMut(&httparse::Response<'_, '_>) -> anyhow::Result<T>,
) -> anyhow::Result<(T, Bytes)> {
    parse_http(stream, |data| {
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut http_res = httparse::Response::new(&mut headers);
        match http_res.parse(data).context("Parsing http response")? {
            httparse::Status::Complete(len) => {
                let result = parser(&http_res)?;
                Ok(Some((result, len)))
            }
            httparse::Status::Partial => Ok(None),
        }
    })
    .await
}

async fn parse_http<T>(
    stream: &mut (impl AsyncRead + Unpin),
    mut parser: impl FnMut(&[u8]) -> anyhow::Result<Option<(T, usize)>>,
) -> anyhow::Result<(T, Bytes)> {
    let mut buf = Cursor::new(vec![0u8; 256]);

    while buf.has_remaining() {
        let byte_read = stream
            .read(buf.remaining_buf())
            .await
            .context("Reading http request")?;
        ensure!(
            byte_read != 0,
            "Connection closed before end of HTTP request"
        );
        buf.advance(byte_read);

        match parser(buf.filled_buf())? {
            Some((result, len)) => {
                let read_position = buf.position() as usize;
                let extra_data = if read_position > len {
                    // We have extra data after the HTTP request, which could be the body, return
                    // this data
                    Bytes::from(buf.into_inner()).slice(len..read_position)
                } else {
                    Bytes::default()
                };

                return Ok((result, extra_data));
            }
            None => {
                if buf.position() == buf.get_ref().len() as u64 {
                    let new_size = (buf.get_ref().len() * 2).max(65536);
                    buf.get_mut().resize(new_size, 0);
                }
            }
        }
    }

    bail!("HTTP head too large")
}

pub trait HttpHeaderExt {
    fn get_header_value(&self, name: &str) -> Option<&[u8]>;
}

impl HttpHeaderExt for [httparse::Header<'_>] {
    fn get_header_value(&self, name: &str) -> Option<&[u8]> {
        self.iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value)
    }
}

pub trait CursorExt {
    fn remaining_buf(&mut self) -> &mut [u8];
    fn filled_buf(&self) -> &[u8];
}

impl CursorExt for Cursor<Vec<u8>> {
    fn remaining_buf(&mut self) -> &mut [u8] {
        let offset = self.position() as usize;
        &mut self.get_mut().as_mut_slice()[offset..]
    }

    fn filled_buf(&self) -> &[u8] {
        let offset = self.position() as usize;
        &self.get_ref().as_slice()[..offset]
    }
}
