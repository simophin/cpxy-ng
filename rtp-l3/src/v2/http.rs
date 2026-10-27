//! Encrypted control envelopes and monotonic, bounded challenge/replay ownership.
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305,
};
use hkdf::Hkdf;
use rand::{rngs::OsRng, Rng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const MAX_BODY: usize = 16384;
pub const CHALLENGE_MS: u64 = 30_000;
pub const RETRY_MS: u64 = 600_000;

pub fn random<const N: usize>() -> [u8; N] {
    let mut b = [0; N];
    OsRng.fill_bytes(&mut b);
    b
}
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}
pub fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut b = [0; N];
    for (i, v) in b.iter_mut().enumerate() {
        *v = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(b)
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bootstrap = 1,
    BootstrapReply = 2,
    Command = 3,
    Reply = 4,
}
impl Kind {
    fn label(self) -> &'static [u8] {
        match self {
            Self::Bootstrap | Self::Command => b"cpxy-rtp/v2/http/client-to-server",
            _ => b"cpxy-rtp/v2/http/server-to-client",
        }
    }
}
pub struct Codec {
    identity: Vec<u8>,
    c2s: XChaCha20Poly1305,
    s2c: XChaCha20Poly1305,
}
impl Codec {
    pub fn new(psk: &[u8; 32], identity: &str) -> Option<Self> {
        if identity.is_empty() || identity.len() > 255 {
            return None;
        }
        let derive = |kind: Kind| {
            let mut key = [0; 32];
            Hkdf::<Sha256>::new(Some(identity.as_bytes()), psk)
                .expand(kind.label(), &mut key)
                .unwrap();
            XChaCha20Poly1305::new((&key).into())
        };
        Some(Self {
            identity: identity.as_bytes().to_vec(),
            c2s: derive(Kind::Command),
            s2c: derive(Kind::Reply),
        })
    }
    fn cipher(&self, kind: Kind) -> &XChaCha20Poly1305 {
        match kind {
            Kind::Bootstrap | Kind::Command => &self.c2s,
            _ => &self.s2c,
        }
    }
    fn aad(&self, kind: Kind) -> Vec<u8> {
        [
            b"cpxy-rtp/v2/http\0".as_slice(),
            &[self.identity.len() as u8],
            &self.identity,
            &[2, kind as u8],
        ]
        .concat()
    }
    pub fn seal(&self, kind: Kind, value: &impl Serialize) -> Option<Vec<u8>> {
        let plaintext = serde_json::to_vec(value).ok()?;
        let nonce = random::<24>();
        let encrypted = self
            .cipher(kind)
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: &plaintext,
                    aad: &self.aad(kind),
                },
            )
            .ok()?;
        let body = [&[2, kind as u8], nonce.as_slice(), &encrypted].concat();
        (body.len() <= MAX_BODY).then_some(body)
    }
    pub fn open<T: serde::de::DeserializeOwned>(&self, kind: Kind, body: &[u8]) -> Option<T> {
        if !(42..=MAX_BODY).contains(&body.len()) || body[..2] != [2, kind as u8] {
            return None;
        }
        let plaintext = self
            .cipher(kind)
            .decrypt(
                body[2..26].into(),
                Payload {
                    msg: &body[26..],
                    aad: &self.aad(kind),
                },
            )
            .ok()?;
        serde_json::from_slice(&plaintext).ok()
    }
}
pub fn hash(body: &[u8]) -> String {
    hex(&Sha256::digest(body))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bootstrap {
    pub token: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapReply {
    pub token: String,
    pub request_hash: String,
    pub generation: String,
    pub challenge: String,
    pub valid_for_ms: u64,
    pub server_age_ms: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rpc {
    pub jsonrpc: String,
    pub id: String,
    pub method: String,
    pub params: serde_json::Map<String, Value>,
}
impl Rpc {
    pub fn valid(&self) -> bool {
        self.jsonrpc == "2.0"
            && !self.id.is_empty()
            && self.id.len() <= 128
            && !self.method.is_empty()
            && self.method.len() <= 64
    }
    pub fn success(&self, result: Value) -> Value {
        json!({"jsonrpc":"2.0","id":self.id,"result":result})
    }
    pub fn error(&self, code: i32, message: &str) -> Value {
        json!({"jsonrpc":"2.0","id":self.id,"error":{"code":code,"message":message}})
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub token: String,
    pub generation: String,
    pub challenge: String,
    pub rpc: Rpc,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub token: String,
    pub request_hash: String,
    pub rpc: Value,
}
struct Challenge {
    expires: u64,
    bootstrap_hash: String,
    bootstrap_reply: Vec<u8>,
    consumed: Option<(String, Vec<u8>)>,
}
/// Single owner calls the handler only once for a valid unconsumed challenge.
/// No logging/Debug implementation on cached plaintext or ciphertext secrets.
pub struct Server {
    pub generation: String,
    codec: Codec,
    limit: usize,
    challenges: BTreeMap<String, Challenge>,
}
impl Server {
    pub fn new(psk: &[u8; 32], identity: &str, limit: usize) -> Option<Self> {
        if limit == 0 {
            return None;
        }
        Some(Self {
            generation: hex(&random::<16>()),
            codec: Codec::new(psk, identity)?,
            limit,
            challenges: BTreeMap::new(),
        })
    }
    /// None means the uniform unauthenticated 404. Handler errors are encrypted.
    pub fn handle(
        &mut self,
        body: &[u8],
        now: u64,
        mut handler: impl FnMut(&Rpc) -> Value,
    ) -> Option<Vec<u8>> {
        self.challenges.retain(|_, c| now < c.expires);
        if body.get(1) == Some(&(Kind::Bootstrap as u8)) {
            let b: Bootstrap = self.codec.open(Kind::Bootstrap, body)?;
            unhex::<16>(&b.token)?;
            let request_hash = hash(body);
            if let Some(c) = self
                .challenges
                .values()
                .find(|c| c.bootstrap_hash == request_hash)
            {
                return Some(c.bootstrap_reply.clone());
            }
            // Bootstrap has no application errors: close/404 at resource capacity.
            if self.challenges.len() >= self.limit {
                return None;
            }
            let challenge = hex(&random::<16>());
            let reply = self.codec.seal(
                Kind::BootstrapReply,
                &BootstrapReply {
                    token: b.token,
                    request_hash: request_hash.clone(),
                    generation: self.generation.clone(),
                    challenge: challenge.clone(),
                    valid_for_ms: CHALLENGE_MS,
                    server_age_ms: now,
                },
            )?;
            self.challenges.insert(
                challenge,
                Challenge {
                    expires: now.saturating_add(CHALLENGE_MS),
                    bootstrap_hash: request_hash,
                    bootstrap_reply: reply.clone(),
                    consumed: None,
                },
            );
            return Some(reply);
        }
        let command: Command = self.codec.open(Kind::Command, body)?;
        unhex::<16>(&command.token)?;
        unhex::<16>(&command.generation)?;
        unhex::<16>(&command.challenge)?;
        if !command.rpc.valid() {
            return None;
        }
        let request_hash = hash(body);
        let error = if command.generation != self.generation {
            Some((-32001, "stale_generation"))
        } else {
            match self.challenges.get(&command.challenge) {
                None => Some((-32002, "stale_challenge")),
                Some(c) => match &c.consumed {
                    Some((h, cached)) if h == &request_hash => return Some(cached.clone()),
                    Some(_) => Some((-32003, "replayed_challenge")),
                    None => None,
                },
            }
        };
        let rpc = match error {
            Some((code, message)) => command.rpc.error(code, message),
            None => handler(&command.rpc),
        };
        let reply = self.codec.seal(
            Kind::Reply,
            &Reply {
                token: command.token,
                request_hash: request_hash.clone(),
                rpc,
            },
        )?;
        if error.is_none() {
            self.challenges.get_mut(&command.challenge)?.consumed =
                Some((request_hash, reply.clone()));
        }
        Some(reply)
    }
}
/// Fresh per HTTP transmission, independent of operation IDs/encrypted tokens.
pub fn random_path() -> String {
    let mut rng = OsRng;
    let length = rng.gen_range(24..=64);
    let segments = rng.gen_range(2..=5);
    let mut cuts = std::collections::BTreeSet::new();
    let chars = length - segments;
    while cuts.len() < segments - 1 {
        cuts.insert(rng.gen_range(1..chars));
    }
    let mut s = String::from("/");
    let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    for i in 0..chars {
        if cuts.contains(&i) {
            s.push('/');
        }
        s.push(alphabet[rng.gen_range(0..alphabet.len())] as char);
    }
    s
}
#[cfg(test)]
mod tests {
    use super::*;
    fn bootstrap(s: &mut Server, c: &Codec, now: u64) -> BootstrapReply {
        let token = hex(&random::<16>());
        let b = c
            .seal(
                Kind::Bootstrap,
                &Bootstrap {
                    token: token.clone(),
                },
            )
            .unwrap();
        let r = s.handle(&b, now, |_| panic!()).unwrap();
        let reply: BootstrapReply = c.open(Kind::BootstrapReply, &r).unwrap();
        assert_eq!(reply.token, token);
        assert_eq!(reply.request_hash, hash(&b));
        reply
    }
    fn command(c: &Codec, b: &BootstrapReply) -> Vec<u8> {
        c.seal(
            Kind::Command,
            &Command {
                token: hex(&random::<16>()),
                generation: b.generation.clone(),
                challenge: b.challenge.clone(),
                rpc: Rpc {
                    jsonrpc: "2.0".into(),
                    id: "1".into(),
                    method: "test".into(),
                    params: Default::default(),
                },
            },
        )
        .unwrap()
    }
    #[test]
    fn challenge_duplicates_consumption_expiry_restart_and_pressure() {
        let mut s = Server::new(&[7; 32], "test", 1).unwrap();
        let c = Codec::new(&[7; 32], "test").unwrap();
        let b = bootstrap(&mut s, &c, 0);
        let cmd = command(&c, &b);
        let mut calls = 0;
        let reply = s
            .handle(&cmd, 1, |rpc| {
                calls += 1;
                rpc.success(json!(true))
            })
            .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(s.handle(&cmd, 2, |_| panic!()), Some(reply));
        let other = command(&c, &b);
        let r = s.handle(&other, 2, |_| panic!()).unwrap();
        let r: Reply = c.open(Kind::Reply, &r).unwrap();
        assert_eq!(r.rpc["error"]["code"], -32003);
        let new = c
            .seal(
                Kind::Bootstrap,
                &Bootstrap {
                    token: hex(&[1; 16]),
                },
            )
            .unwrap();
        assert!(s.handle(&new, 3, |_| panic!()).is_none());
        let r = s.handle(&cmd, CHALLENGE_MS, |_| panic!()).unwrap();
        let r: Reply = c.open(Kind::Reply, &r).unwrap();
        assert_eq!(r.rpc["error"]["code"], -32002);
        let mut restarted = Server::new(&[7; 32], "test", 1).unwrap();
        let r = restarted.handle(&cmd, 1, |_| panic!()).unwrap();
        let r: Reply = c.open(Kind::Reply, &r).unwrap();
        assert_eq!(r.rpc["error"]["code"], -32001);
        assert!(s.handle(&new, CHALLENGE_MS, |_| panic!()).is_some());
    }
    #[test]
    fn malformed_and_tampered_content_is_uniformly_rejected() {
        let mut s = Server::new(&[7; 32], "test", 4).unwrap();
        let c = Codec::new(&[7; 32], "test").unwrap();
        let b = bootstrap(&mut s, &c, 0);
        let cmd = command(&c, &b);
        for i in 0..cmd.len() {
            let mut q = cmd.clone();
            q[i] ^= 1;
            assert!(s.handle(&q, 1, |_| panic!()).is_none());
        }
        let mut v: Value = c.open(Kind::Command, &cmd).unwrap();
        v["rpc"]["id"] = Value::Null;
        let q = c.seal(Kind::Command, &v).unwrap();
        assert!(s.handle(&q, 1, |_| panic!()).is_none());
        v["rpc"]["id"] = json!("1");
        v["extra"] = json!(1);
        let q = c.seal(Kind::Command, &v).unwrap();
        assert!(s.handle(&q, 1, |_| panic!()).is_none());
        assert!(Codec::new(&[7; 32], "other")
            .unwrap()
            .open::<Value>(Kind::Command, &cmd)
            .is_none());
        assert!(c.open::<Value>(Kind::Reply, &cmd).is_none());
    }
    #[test]
    fn bootstrap_replay_does_not_allocate_another_challenge() {
        let mut s = Server::new(&[7; 32], "test", 1).unwrap();
        let c = Codec::new(&[7; 32], "test").unwrap();
        let b = c
            .seal(
                Kind::Bootstrap,
                &Bootstrap {
                    token: hex(&[1; 16]),
                },
            )
            .unwrap();
        let r = s.handle(&b, 0, |_| panic!()).unwrap();
        assert_eq!(s.handle(&b, 1, |_| panic!()), Some(r));
        assert_eq!(s.challenges.len(), 1);
    }
    #[test]
    fn paths_satisfy_policy_without_fixed_prefix() {
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..1000 {
            let p = random_path();
            assert!((24..=64).contains(&p.len()));
            let parts: Vec<_> = p[1..].split('/').collect();
            assert!((2..=5).contains(&parts.len()));
            assert!(parts
                .iter()
                .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())));
            assert!(seen.insert(p));
        }
    }
}
