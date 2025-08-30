use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use chacha20::ChaCha20;
use rand::random;
use rkyv::{Archive, Deserialize, Serialize};
use std::io::Error;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub enum Configuration {
    Plaintext,
    PartialEncrypt {
        key: [u8; 32],
        nonce: [u8; 12],
        enc_size: NonZeroUsize,
    },
    FullEncrypt {
        key: [u8; 32],
        nonce: [u8; 12],
    },
}

impl Configuration {
    pub fn random_full() -> Self {
        Self::FullEncrypt {
            key: random(),
            nonce: random(),
        }
    }

    pub fn random_partial(enc_size: NonZeroUsize) -> Self {
        Self::PartialEncrypt {
            key: random(),
            nonce: random(),
            enc_size,
        }
    }
}

enum CipherState<B> {
    None,
    Partial {
        remaining: NonZeroUsize,
        cipher: ChaCha20,
        buffer: B,
    },
    Full(ChaCha20, B),
}

impl<B> CipherState<B> {
    fn new(config: &Configuration, buffer: B) -> Self {
        match config {
            Configuration::Plaintext => Self::None,
            Configuration::PartialEncrypt { key, nonce, enc_size } => {
                let cipher = ChaCha20::new(key.into(), nonce.into());
                Self::Partial {
                    remaining: *enc_size,
                    cipher,
                    buffer,
                }
            },
            Configuration::FullEncrypt { key, nonce } => {
                let cipher = ChaCha20::new(key.into(), nonce.into());
                Self::Full(cipher, buffer)
            },
        }
    }
}

impl<B> Default for CipherState<B> {
    fn default() -> Self {
        Self::None
    }
}

pub struct CipherStream<S> {
    encrypt_state: CipherState<()>,
    decrypt_state: CipherState<Vec<u8>>,
    stream: S,
}

impl<S> CipherStream<S> {
    pub fn new(stream: S, encrypt_config: &Configuration, decrypt_config: &Configuration) -> Self {
        Self {
            encrypt_state: CipherState::new(encrypt_config, ()),
            decrypt_state: CipherState::new(decrypt_config, vec![0u8; 8192]),
            stream,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for CipherStream<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let old_filled_size = buf.filled().len();
        ready!(Pin::new(&mut self.stream).poll_read(cx, buf))?;
        let filled = &mut buf.filled_mut()[old_filled_size..];

        if filled.is_empty() {
            return Poll::Ready(Ok(()));
        }

        match &mut self.encrypt_state {
            CipherState::Full(cipher, ..) => {
                cipher.apply_keystream(filled);
            },
            CipherState::Partial { remaining, cipher, .. } => {
                let transform_size = remaining.get().min(filled.len());
                cipher.apply_keystream(&mut filled[..transform_size]);

                match NonZeroUsize::new(remaining.get() - transform_size) {
                    Some(v) => *remaining = v,
                    None => self.encrypt_state = CipherState::None,
                }
            },
            CipherState::None => {},
        }

        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for CipherStream<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Error>> {
        if buf.is_empty() {
            return Pin::new(&mut self.stream).poll_write(cx, buf);
        }

        let mut decrypt_state = std::mem::take(&mut self.decrypt_state);
        let (cipher, enc_len, enc_buf) = match &mut decrypt_state {
            CipherState::Full(cipher, b) => (cipher, buf.len(), b),
            CipherState::Partial { remaining, cipher, buffer } => (cipher, remaining.get().min(buf.len()), buffer),
            CipherState::None => return Pin::new(&mut self.stream).poll_write(cx, buf),
        };

        let enc_len = enc_len.min(enc_buf.len());
        cipher.apply_keystream_b2b(&buf[..enc_len], &mut enc_buf[..enc_len])
            .map_err(|e| Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        let ret = Pin::new(&mut self.stream).poll_write(cx, &enc_buf[..enc_len]);

        let byte_written = match &ret {
            Poll::Pending => 0,
            Poll::Ready(Ok(v)) => *v,
            Poll::Ready(Err(_)) => return ret,
        };

        if byte_written < enc_len {
            // We only wrote part of the encrypted data, need to wind back the cipher
            cipher.try_seek(cipher.current_pos::<u64>() - (enc_len - byte_written) as u64)
                .map_err(|e| Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        }

        self.decrypt_state = match decrypt_state {
            CipherState::Partial { remaining, cipher, buffer } => {
                match NonZeroUsize::new(remaining.get() - byte_written) {
                    Some(v) => CipherState::Partial { remaining: v, cipher, buffer },
                    None => CipherState::None,
                }
            },
            v => v,
        };

        ret
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn test_case(
        name: &str,
        client_send_config: Configuration,
        server_send_config: Configuration,
    ) {
        let (client, server) =  tokio::io::duplex(32);
        let mut client_stream = CipherStream::new(client, &client_send_config, &server_send_config);
        let mut server_stream = CipherStream::new(server, &server_send_config, &client_send_config);

        let test_data = b"The quick brown fox jumps over the lazy dog";

        let received_data = tokio::spawn(async move {
            let mut buf = vec![0u8; test_data.len()];
            server_stream.read_exact(&mut buf).await.unwrap();
            buf
        });

        println!("Test case: {}", name);
        client_stream.write_all(test_data).await.unwrap();

        let received_data = received_data.await.unwrap();
        assert_eq!(test_data, &received_data[..], "Data mismatch in test case: {}", name);
    }

    #[tokio::test]
    async fn test_cipher_stream() {
        test_case(
            "Plaintext to Partial",
            Configuration::Plaintext,
            Configuration::random_partial(NonZeroUsize::new(4).unwrap()),
        ).await;

        test_case(
            "Plaintext to Full Encrypt",
            Configuration::Plaintext,
            Configuration::random_full(),
        ).await;

        test_case(
            "Partial to Partial",
            Configuration::random_partial(NonZeroUsize::new(128).unwrap()),
            Configuration::random_partial(NonZeroUsize::new(16).unwrap()),
        ).await;
    }
}