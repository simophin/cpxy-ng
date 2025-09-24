use anyhow::{Context, ensure, format_err};
use chacha20::cipher::crypto_common::rand_core::OsRng;
use chacha20poly1305::aead::AeadMut;
use chacha20poly1305::{AeadCore, Key, KeyInit, XChaCha20Poly1305};
use rkyv::api::high::HighValidator;
use rkyv::bytecheck::CheckBytes;
use rkyv::de::Pool;
use rkyv::rancor::{Error, Strategy};
use rkyv::ser::Serializer;
use rkyv::ser::allocator::ArenaHandle;
use rkyv::ser::sharing::Share;
use rkyv::util::AlignedVec;
use rkyv::{Archive, Deserialize, Serialize};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::Duration;

pub trait DnsModel: Sized {
    fn encrypt(&self, key: &Key) -> anyhow::Result<Vec<u8>>
    where
        for<'a> Self: Serialize<Strategy<Serializer<AlignedVec, ArenaHandle<'a>, Share>, Error>>,
    {
        let mut cipher = XChaCha20Poly1305::new(key);
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let mut cipher_text = cipher
            .encrypt(
                &nonce,
                rkyv::to_bytes::<Error>(self)
                    .context("Serializing model")?
                    .as_ref(),
            )
            .map_err(|e| format_err!("Error encrypting: {e:?}"))?;

        cipher_text.extend_from_slice(&nonce);
        Ok(cipher_text)
    }

    fn decrypt(key: &Key, data: &[u8]) -> anyhow::Result<Self>
    where
        Self: Archive,
        for<'a> <Self as Archive>::Archived:
            CheckBytes<HighValidator<'a, Error>> + Deserialize<Self, Strategy<Pool, Error>>,
    {
        ensure!(data.len() > 24, "Data too short");
        let (cipher_text, nonce) = data.split_at(data.len() - 24);
        let plain_text = XChaCha20Poly1305::new(key)
            .decrypt(nonce.into(), cipher_text)
            .map_err(|e| format_err!("Error decrypting: {e:?}"))?;

        rkyv::from_bytes::<Self, Error>(&plain_text).context("Error deserializing model")
    }
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize, Eq, PartialEq)]
pub enum Request {
    DnsResolve(Vec<String>),
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize, Eq, PartialEq, Default)]
pub struct SingleResolveResult {
    pub addresses: Vec<Ipv4Addr>,
    pub ttl: Duration,
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize, Eq, PartialEq, Default)]
pub struct DnsResolveResult {
    pub result: HashMap<String, SingleResolveResult>,
}

impl DnsModel for Request {}
impl DnsModel for DnsResolveResult {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_works() {
        let key = Key::from([1u8; 32]);
        let request = Request::DnsResolve(vec!["example.com".to_string()]);
        let encrypted = request.encrypt(&key).expect("Encryption failed");
        let decrypted = Request::decrypt(&key, &encrypted).expect("Decryption failed");
        assert_eq!(request, decrypted);

        let result = DnsResolveResult {
            result: HashMap::from([(
                "example.com".to_string(),
                SingleResolveResult {
                    addresses: vec![Ipv4Addr::new(93, 184, 216, 34)],
                    ttl: Duration::from_secs(24 * 60 * 60),
                },
            )]),
        };
        let encrypted = result.encrypt(&key).expect("Encryption failed");
        let decrypted = DnsResolveResult::decrypt(&key, &encrypted).expect("Decryption failed");
        assert_eq!(result, decrypted);
    }
}
