//! The messages of the cpxy protocol, and how they are sealed with the pre-shared key.
//!
//! A connection to the server opens with one [`ClientMessage`] (sealed into the URL path of an
//! HTTP WebSocket upgrade, see [`crate::http_protocol`]) and the server answers with one
//! [`ServerMessage`]. After an accepted [`ClientMessage::Connect`] the connection relays to the
//! target. After an accepted [`ClientMessage::Register`] it becomes a reverse link: the server
//! later sends a [`LinkCommand`] down it and the reverse exit answers with a [`DialReply`], as
//! length-prefixed frames ([`write_frame`], [`read_frame`]), then it relays like any tunnel.
//!
//! # Changing the messages
//!
//! Messages are archived with rkyv, which has no field-level versioning, so:
//!
//! - Every message is an enum whose variants each hold a `Box`. The archived enum is then a tag
//!   and a relative pointer, the same size whatever the variants hold, so a variant can be added
//!   without moving the others, and a peer that doesn't know it rejects it cleanly as an invalid
//!   discriminant. An unboxed variant would change the enum's size and make older peers misread
//!   every message.
//! - The structs inside never change. To change a message, add a variant with a new struct.
//!
//! The tests pin both rules: the archived sizes, and the exact bytes of every variant.

pub mod legacy;

use crate::encrypt_stream::Configuration;
use anyhow::{Context, ensure, format_err};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, OsRng};
use chacha20poly1305::{AeadCore, Key, KeyInit, XChaCha20Poly1305};
use rkyv::api::high::{HighSerializer, HighValidator};
use rkyv::bytecheck::CheckBytes;
use rkyv::de::Pool;
use rkyv::rancor::{Error as RkyvError, Strategy};
use rkyv::ser::allocator::ArenaHandle;
use rkyv::util::AlignedVec;
use rkyv::{Archive, Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// What a client opens a connection with.
#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub enum ClientMessage {
    /// Connect to a target and relay to it.
    Connect(Box<Connect>),
    /// Offer this connection as a link of a reverse exit.
    Register(Box<Register>),
}

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct Connect {
    pub host: String,
    pub port: u16,
    pub client_send_cipher: Configuration,
    pub server_send_cipher: Configuration,
    /// Sent to the target as soon as the server connects to it.
    pub initial_plaintext: Vec<u8>,
}

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct Register {
    /// The reverse exit this link belongs to, as named in the server's `reverse://` routes.
    pub name: String,
    pub client_send_cipher: Configuration,
    pub server_send_cipher: Configuration,
}

/// The server's answer to a [`ClientMessage`].
#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub enum ServerMessage {
    /// For a `Connect`: the target is connected, relaying starts. For a `Register`: the link is
    /// parked until the server needs it.
    Accepted(Box<Accepted>),
    Rejected(Box<Rejected>),
}

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct Accepted {}

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct Rejected {
    pub msg: String,
}

impl ServerMessage {
    pub fn accepted() -> Self {
        Self::Accepted(Box::new(Accepted {}))
    }

    pub fn rejected(msg: impl Into<String>) -> Self {
        Self::Rejected(Box::new(Rejected { msg: msg.into() }))
    }
}

/// The server's instruction down a parked reverse link.
#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub enum LinkCommand {
    /// Connect to a target, answer with a [`DialReply`], then relay the link to it.
    Dial(Box<Dial>),
}

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct Dial {
    pub host: String,
    pub port: u16,
}

/// The reverse exit's answer to a [`LinkCommand::Dial`].
#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub enum DialReply {
    Connected(Box<Connected>),
    Failed(Box<Failed>),
}

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct Connected {}

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct Failed {
    pub msg: String,
}

/// Which generation of messages a connection opened with. The server answers in the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Current,
    Legacy,
}

/// A client's opening message, in either generation.
#[derive(Debug, PartialEq, Clone)]
pub enum ClientEnvelope {
    Current(ClientMessage),
    Legacy(legacy::Request),
}

/// A server's answer, in either generation.
#[derive(Debug, PartialEq, Clone)]
pub enum ServerEnvelope {
    Current(ServerMessage),
    Legacy(legacy::Response),
}

impl ClientEnvelope {
    pub fn format(&self) -> Format {
        match self {
            Self::Current(_) => Format::Current,
            Self::Legacy(_) => Format::Legacy,
        }
    }

    /// Encrypts the message into URL path text: unpadded URL-safe base64 with `/` scattered
    /// through it, so it reads as path segments.
    pub fn seal(&self, key: &Key) -> anyhow::Result<String> {
        let bytes = match self {
            Self::Current(msg) => encode(msg),
            Self::Legacy(req) => encode(req),
        }
        .context("Error serializing request")?;

        let mut url = URL_SAFE_NO_PAD.encode(secret_box_encrypt(key, &bytes)?);
        let mut pos = 1;
        while pos < url.len() - 2 {
            let insertion_point = rand::random_range(pos..url.len());
            url.insert(insertion_point, '/');
            pos = insertion_point + 2;
        }
        Ok(url)
    }

    /// Reverses [`Self::seal`]. The two generations can't be mistaken for each other: a legacy
    /// request ends with its timestamp, whose upper half is zero until 2106, so read as a
    /// [`ClientMessage`] its variant points at itself and fails validation. The tests pin this.
    pub fn open(text: &str, key: &Key) -> anyhow::Result<Self> {
        let bytes = URL_SAFE_NO_PAD
            .decode(text.replace('/', ""))
            .map_err(|e| format_err!("Error base64 decoding request from URL path: {e}"))?;
        let bytes = secret_box_decrypt(key, &bytes).context("Error decrypting request")?;

        match decode::<ClientMessage>(&bytes) {
            Ok(msg) => Ok(Self::Current(msg)),
            Err(current) => decode::<legacy::Request>(&bytes)
                .map(Self::Legacy)
                .with_context(|| format!("Request is neither current ({current:#}) nor legacy")),
        }
    }
}

impl ServerEnvelope {
    pub fn seal(&self, key: &Key) -> anyhow::Result<Vec<u8>> {
        let bytes = match self {
            Self::Current(msg) => encode(msg),
            Self::Legacy(res) => encode(res),
        }
        .context("Error serializing response")?;
        secret_box_encrypt(key, &bytes)
    }

    /// Reverses [`Self::seal`], expecting the generation the client opened with.
    pub fn open(data: &[u8], key: &Key, format: Format) -> anyhow::Result<Self> {
        let bytes = secret_box_decrypt(key, data).context("Error decrypting response")?;
        match format {
            Format::Current => decode(&bytes).map(Self::Current),
            Format::Legacy => decode(&bytes).map(Self::Legacy),
        }
        .context("Error deserializing response")
    }
}

/// A reverse exit replaces a parked link that has gone unused this long, staying under the idle
/// timeouts of NATs and firewalls between it and the server. The server drops links older than
/// this, with some slack.
pub const REVERSE_LINK_MAX_IDLE: Duration = Duration::from_secs(20);

/// The largest frame [`read_frame`] accepts.
const MAX_FRAME_LEN: usize = 64 * 1024;

/// Writes `msg` as a frame: its length as a big-endian `u32`, then its archived bytes.
pub async fn write_frame<T>(stream: &mut (impl AsyncWrite + Unpin), msg: &T) -> anyhow::Result<()>
where
    T: for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, RkyvError>>,
{
    let bytes = encode(msg)?;
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&u32::try_from(bytes.len())?.to_be_bytes());
    frame.extend_from_slice(&bytes);
    stream
        .write_all(&frame)
        .await
        .context("Error writing frame")?;
    stream.flush().await.context("Error flushing frame")
}

/// Reads a frame written by [`write_frame`].
pub async fn read_frame<T>(stream: &mut (impl AsyncRead + Unpin)) -> anyhow::Result<T>
where
    T: Archive,
    T::Archived: for<'a> CheckBytes<HighValidator<'a, RkyvError>>
        + Deserialize<T, Strategy<Pool, RkyvError>>,
{
    let len = stream
        .read_u32()
        .await
        .context("Error reading frame length")? as usize;
    ensure!(len <= MAX_FRAME_LEN, "Frame of {len} bytes is too large");
    let mut bytes = vec![0u8; len];
    stream
        .read_exact(&mut bytes)
        .await
        .context("Error reading frame")?;
    decode(&bytes)
}

fn encode<T>(value: &T) -> anyhow::Result<AlignedVec>
where
    T: for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, RkyvError>>,
{
    Ok(rkyv::to_bytes::<RkyvError>(value)?)
}

fn decode<T>(bytes: &[u8]) -> anyhow::Result<T>
where
    T: Archive,
    T::Archived: for<'a> CheckBytes<HighValidator<'a, RkyvError>>
        + Deserialize<T, Strategy<Pool, RkyvError>>,
{
    // rkyv reads the archive in place, so it must be suitably aligned.
    let mut aligned = AlignedVec::<16>::with_capacity(bytes.len());
    aligned.extend_from_slice(bytes);
    Ok(rkyv::from_bytes::<T, RkyvError>(&aligned)?)
}

fn secret_box_encrypt(key: &Key, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let cipher = XChaCha20Poly1305::new(key);
    let mut ciphertext = cipher
        .encrypt((&nonce).into(), plaintext)
        .map_err(|e| format_err!("Error encrypting data: {e}"))?;

    // Prepend nonce to the ciphertext
    ciphertext.splice(0..0, nonce.iter().copied());
    Ok(ciphertext)
}

fn secret_box_decrypt(key: &Key, ciphertext: &[u8]) -> anyhow::Result<Vec<u8>> {
    ensure!(
        ciphertext.len() >= 24,
        "Ciphertext too short to contain nonce"
    );
    let (nonce_bytes, ciphertext) = ciphertext.split_at(24); // XChaCha20Poly1305 nonce size is 24 bytes

    let cipher = XChaCha20Poly1305::new(key);
    cipher
        .decrypt(nonce_bytes.into(), ciphertext)
        .map_err(|e| format_err!("Error decrypting: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cipher_select::select_cipher_based_on_port;
    use std::num::NonZeroUsize;

    fn key() -> Key {
        XChaCha20Poly1305::generate_key(&mut OsRng)
    }

    /// Fixed ciphers, so archived bytes are reproducible.
    fn partial() -> Configuration {
        Configuration::PartialEncrypt {
            key: [1; 32],
            nonce: [2; 12],
            enc_size: NonZeroUsize::new(32).unwrap(),
        }
    }

    fn full() -> Configuration {
        Configuration::FullEncrypt {
            key: [3; 32],
            nonce: [4; 12],
        }
    }

    fn connect() -> ClientMessage {
        ClientMessage::Connect(Box::new(Connect {
            host: "example.com".into(),
            port: 443,
            client_send_cipher: partial(),
            server_send_cipher: full(),
            initial_plaintext: vec![0x16, 3, 1],
        }))
    }

    fn register() -> ClientMessage {
        ClientMessage::Register(Box::new(Register {
            name: "home".into(),
            client_send_cipher: full(),
            server_send_cipher: full(),
        }))
    }

    fn legacy_request(host: &str, port: u16, initial_plaintext: Vec<u8>) -> legacy::Request {
        let (client_send_cipher, server_send_cipher) = select_cipher_based_on_port(port);
        legacy::Request {
            host: host.into(),
            port,
            tls: false,
            client_send_cipher,
            server_send_cipher,
            initial_plaintext,
            timestamp_epoch_seconds: 0,
        }
    }

    #[test]
    fn client_envelopes_round_trip() {
        let key = key();
        for envelope in [
            ClientEnvelope::Current(connect()),
            ClientEnvelope::Current(register()),
            ClientEnvelope::Legacy(legacy_request("example.com", 443, b"hello".to_vec())),
        ] {
            let sealed = envelope.seal(&key).unwrap();
            assert_eq!(ClientEnvelope::open(&sealed, &key).unwrap(), envelope);
        }
    }

    #[test]
    fn server_envelopes_round_trip() {
        let key = key();
        for (envelope, format) in [
            (
                ServerEnvelope::Current(ServerMessage::accepted()),
                Format::Current,
            ),
            (
                ServerEnvelope::Current(ServerMessage::rejected("no")),
                Format::Current,
            ),
            (
                ServerEnvelope::Legacy(legacy::Response::Success {
                    initial_response: b"hi".to_vec(),
                    timestamp_epoch_seconds: 1,
                }),
                Format::Legacy,
            ),
        ] {
            let sealed = envelope.seal(&key).unwrap();
            assert_eq!(
                ServerEnvelope::open(&sealed, &key, format).unwrap(),
                envelope
            );
        }
    }

    #[test]
    fn wrong_key_is_rejected() {
        let sealed = ClientEnvelope::Current(connect()).seal(&key()).unwrap();
        assert!(ClientEnvelope::open(&sealed, &key()).is_err());
    }

    /// Legacy requests must never decode as current messages, and the reverse, whatever they
    /// carry. The first is guaranteed by the layout (see [`ClientEnvelope::open`]); this checks it.
    #[test]
    fn generations_are_never_confused() {
        for host in [
            "a.cn",
            "www.example.com",
            "2001:db8::1",
            "a-rather-long-host.example.org",
        ] {
            for port in [22, 80, 443, 8443] {
                for initial in [vec![], vec![0x16, 3, 1], vec![0x16; 1500]] {
                    let mut req = legacy_request(host, port, initial.clone());
                    for timestamp in [0, 1_800_000_000] {
                        req.timestamp_epoch_seconds = timestamp;
                        let bytes = encode(&req).unwrap();
                        assert!(
                            decode::<ClientMessage>(&bytes).is_err(),
                            "legacy {req:?} decoded as current"
                        );
                    }

                    let (client_send_cipher, server_send_cipher) =
                        select_cipher_based_on_port(port);
                    for msg in [
                        ClientMessage::Connect(Box::new(Connect {
                            host: host.into(),
                            port,
                            client_send_cipher: client_send_cipher.clone(),
                            server_send_cipher: server_send_cipher.clone(),
                            initial_plaintext: initial.clone(),
                        })),
                        ClientMessage::Register(Box::new(Register {
                            name: host.into(),
                            client_send_cipher,
                            server_send_cipher,
                        })),
                    ] {
                        let bytes = encode(&msg).unwrap();
                        assert!(
                            decode::<legacy::Request>(&bytes).is_err(),
                            "current {msg:?} decoded as legacy"
                        );
                    }
                }
            }
        }
    }

    /// Every message must stay a tag and a relative pointer: see the module docs.
    #[test]
    fn messages_are_boxed() {
        assert_eq!(size_of::<ArchivedClientMessage>(), 8);
        assert_eq!(size_of::<ArchivedServerMessage>(), 8);
        assert_eq!(size_of::<ArchivedLinkCommand>(), 8);
        assert_eq!(size_of::<ArchivedDialReply>(), 8);
    }

    /// The archived bytes of every variant. If this fails, a message's layout changed, which
    /// breaks every deployed peer: add a variant instead (see the module docs).
    #[test]
    fn archived_bytes_are_stable() {
        fn hex<T>(msg: &T) -> String
        where
            T: for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, RkyvError>>,
        {
            encode(msg)
                .unwrap()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        }

        let cases = [
            ("connect", hex(&connect()), CONNECT_HEX),
            ("register", hex(&register()), REGISTER_HEX),
            ("accepted", hex(&ServerMessage::accepted()), ACCEPTED_HEX),
            (
                "rejected",
                hex(&ServerMessage::rejected("no exit")),
                REJECTED_HEX,
            ),
            (
                "dial",
                hex(&LinkCommand::Dial(Box::new(Dial {
                    host: "example.com".into(),
                    port: 443,
                }))),
                DIAL_HEX,
            ),
            (
                "connected",
                hex(&DialReply::Connected(Box::new(Connected {}))),
                CONNECTED_HEX,
            ),
            (
                "failed",
                hex(&DialReply::Failed(Box::new(Failed {
                    msg: "refused".into(),
                }))),
                FAILED_HEX,
            ),
        ];
        let changed: Vec<_> = cases
            .iter()
            .filter(|(_, actual, expected)| actual != expected)
            .map(|(name, actual, _)| format!("{name}: {actual}"))
            .collect();
        assert!(
            changed.is_empty(),
            "archived bytes changed:\n{}",
            changed.join("\n")
        );
    }

    const CONNECT_HEX: &str = "6578616d706c652e636f6d16030100008b000000f0ffffffbb010000010101010101010101010101010101010101010101010101010101010101010101020202020202020202020202000000200000000203030303030303030303030303030303030303030303030303030303030303030404040404040404040404040000000000000087ffffff030000000000000080ffffff";
    const REGISTER_HEX: &str = "686f6d65ffffffff0203030303030303030303030303030303030303030303030303030303030303030404040404040404040404040000000000000002030303030303030303030303030303030303030303030303030303030303030304040404040404040404040400000000000000010000008cffffff";
    const ACCEPTED_HEX: &str = "00000000fcffffff";
    const REJECTED_HEX: &str = "6e6f2065786974ff01000000f4ffffff";
    const DIAL_HEX: &str = "6578616d706c652e636f6d008b000000f4ffffffbb01000000000000f0ffffff";
    const CONNECTED_HEX: &str = "00000000fcffffff";
    const FAILED_HEX: &str = "72656675736564ff01000000f4ffffff";

    #[tokio::test]
    async fn frames_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let dial = LinkCommand::Dial(Box::new(Dial {
            host: "example.com".into(),
            port: 80,
        }));
        write_frame(&mut a, &dial).await.unwrap();
        assert_eq!(read_frame::<LinkCommand>(&mut b).await.unwrap(), dial);
    }

    #[tokio::test]
    async fn oversized_frames_are_refused() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&(MAX_FRAME_LEN as u32 + 1).to_be_bytes())
            .await
            .unwrap();
        assert!(read_frame::<LinkCommand>(&mut b).await.is_err());
    }
}
