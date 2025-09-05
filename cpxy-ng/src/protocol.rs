use crate::encrypt_stream::Configuration;
use anyhow::{Context, format_err, ensure};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, OsRng};
use chacha20poly1305::{AeadCore, Key, KeyInit, XChaCha20Poly1305};
use rkyv::rancor::Error as RkyvError;
use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct Request {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub client_send_cipher: Configuration,
    pub server_send_cipher: Configuration,
    pub initial_plaintext: Vec<u8>,
    pub timestamp_epoch_seconds: u64,
}

fn secret_box_encrypt(key: &Key, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let cipher = chacha20poly1305::XChaCha20Poly1305::new(key);
    let mut ciphertext = cipher
        .encrypt((&nonce).into(), plaintext)
        .map_err(|e| format_err!("Error encrypting data: {e}"))?;

    // Prepend nonce to the ciphertext
    ciphertext.splice(0..0, nonce.iter().copied());
    Ok(ciphertext)
}

fn secret_box_decrypt(key: &Key, ciphertext: &[u8]) -> anyhow::Result<Vec<u8>> {
    ensure!(ciphertext.len() >= 24, "Ciphertext too short to contain nonce");
    let (nonce_bytes, ciphertext) = ciphertext.split_at(24); // XChaCha20Poly1305 nonce size is 24 bytes

    let cipher = chacha20poly1305::XChaCha20Poly1305::new(key);
    cipher
        .decrypt(nonce_bytes.into(), ciphertext)
        .map_err(|e| format_err!("Error decrypting request: {e}"))
}

impl Request {
    pub fn serialize_as_url_path_segments(&self, encrypt_key: &Key) -> anyhow::Result<String> {
        let bytes = rkyv::to_bytes::<RkyvError>(self).context("Error serializing request")?;
        let bytes = secret_box_encrypt(encrypt_key, &bytes)?;

        let mut url = URL_SAFE_NO_PAD.encode(bytes);
        let mut pos = 1;
        while pos < url.len() - 2 {
            let insertion_point = rand::random_range(pos..url.len());
            url.insert(insertion_point, '/');
            pos = insertion_point + 2;
        }

        Ok(url)
    }

    pub fn deserialize_from_url_path_segments(
        path: &str,
        encrypt_key: &Key,
    ) -> anyhow::Result<Self> {
        let url = path.replace('/', "");
        let bytes = URL_SAFE_NO_PAD.decode(url).map_err(|e| {
            format_err!("Error base64 decoding request from URL path segments: {e}")
        })?;

        let bytes = secret_box_decrypt(encrypt_key, &bytes)?;
        rkyv::from_bytes::<Self, RkyvError>(&bytes).context("Error deserializing request")
    }
}

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub enum Response {
    Success {
        initial_response: Vec<u8>,
        timestamp_epoch_seconds: u64,
    },

    Error {
        msg: String,
        timestamp_epoch_seconds: u64,
    },
}

impl Response {
    pub fn serialize(&self, encrypt_key: &Key) -> anyhow::Result<Vec<u8>> {
        let bytes = rkyv::to_bytes::<RkyvError>(self).context("Error serializing response")?;
        secret_box_encrypt(encrypt_key, &bytes)
    }

    pub fn deserialize(data: &[u8], encrypt_key: &Key) -> anyhow::Result<Self> {
        let bytes = secret_box_decrypt(encrypt_key, data)?;
        rkyv::from_bytes::<Self, RkyvError>(&bytes).context("Error deserializing response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chacha20poly1305::ChaCha20Poly1305;

    #[test]
    fn test_request_serialization() {
        let request = Request {
            host: "example.com".to_string(),
            port: 8080,
            client_send_cipher: Configuration::random_full(),
            server_send_cipher: Configuration::random_full(),
            initial_plaintext: b"Hello, World!".to_vec(),
            timestamp_epoch_seconds: 0,
            tls: false,
        };

        let key = ChaCha20Poly1305::generate_key(&mut OsRng);

        let url_path = request.serialize_as_url_path_segments(&key).unwrap();
        let deserialized_request =
            Request::deserialize_from_url_path_segments(&url_path, &key).unwrap();

        assert_eq!(request, deserialized_request);
    }

    #[test]
    fn test_response_serialization() {
        let response = Response::Success {
            timestamp_epoch_seconds: 0,
            initial_response: b"Hello, Client!".to_vec(),
        };

        let key = ChaCha20Poly1305::generate_key(&mut OsRng);

        let serialized_response = response.serialize(&key).unwrap();
        let deserialized_response = Response::deserialize(&serialized_response, &key).unwrap();

        assert_eq!(response, deserialized_response);
    }
}
