//! Allocation-scoped AEAD, IPv4 header compression and bounded control encoding.
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305,
};
use hkdf::Hkdf;
use rand::{rngs::OsRng, RngCore};
use sha2::Sha256;
use std::net::Ipv4Addr;

pub const OVERHEAD: usize = 36;
pub const MAX_FRAME: usize = crate::MTU + OVERHEAD - 4;
const CONTEXT: &[u8] = b"cpxy-rtp/v2/udp\0";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Upstream,
    Downstream,
}
impl Direction {
    fn label(self) -> &'static [u8] {
        match self {
            Self::Upstream => b"cpxy-rtp/v2/udp/client-to-server",
            Self::Downstream => b"cpxy-rtp/v2/udp/server-to-client",
        }
    }
    fn offset(self) -> usize {
        if self == Self::Upstream {
            12
        } else {
            16
        }
    }
}

// Deliberately not Clone or Debug: one counter owner per directional key.
pub struct Tx {
    cipher: XChaCha20Poly1305,
    prefix: [u8; 16],
    allocation: [u8; 16],
    next: Option<u64>,
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
    direction: Direction,
    inner: Ipv4Addr,
}
pub struct Rx {
    cipher: XChaCha20Poly1305,
    prefix: [u8; 16],
    allocation: [u8; 16],
    replay: Replay,
    direction: Direction,
    inner: Ipv4Addr,
}
fn material(
    secret: &[u8; 32],
    allocation: &[u8; 16],
    direction: Direction,
) -> ([u8; 32], [u8; 16]) {
    let mut output = [0; 48];
    Hkdf::<Sha256>::new(Some(allocation), secret)
        .expand(direction.label(), &mut output)
        .unwrap();
    (
        output[..32].try_into().unwrap(),
        output[32..].try_into().unwrap(),
    )
}
fn nonce(prefix: &[u8; 16], number: u64) -> [u8; 24] {
    let mut nonce = [0; 24];
    nonce[..16].copy_from_slice(prefix);
    nonce[16..].copy_from_slice(&number.to_be_bytes());
    nonce
}
fn aad(allocation: &[u8; 16], header: &[u8]) -> Vec<u8> {
    [CONTEXT, allocation, header].concat()
}
impl Tx {
    pub fn new(
        secret: &[u8; 32],
        allocation: [u8; 16],
        direction: Direction,
        inner: Ipv4Addr,
    ) -> Self {
        let (key, prefix) = material(secret, &allocation, direction);
        Self {
            cipher: XChaCha20Poly1305::new((&key).into()),
            prefix,
            allocation,
            next: Some(0),
            ssrc: OsRng.next_u32(),
            sequence: OsRng.next_u32() as u16,
            timestamp: OsRng.next_u32(),
            direction,
            inner,
        }
    }
    pub fn data(&mut self, ip: &[u8], ticks: u32) -> Option<Vec<u8>> {
        if !crate::ipv4(ip) {
            return None;
        }
        let offset = self.direction.offset();
        if ip[offset..offset + 4] != self.inner.octets() {
            return None;
        }
        let body = [&ip[..offset], &ip[offset + 4..]].concat();
        self.seal(&body, ticks)
    }
    pub fn control(&mut self, control: Control, ticks: u32) -> Option<Vec<u8>> {
        self.seal(&control.encode(), ticks)
    }
    fn seal(&mut self, body: &[u8], ticks: u32) -> Option<Vec<u8>> {
        let number = self.next?;
        // Consume before encryption: failure cannot cause nonce reuse.
        self.next = number.checked_add(1);
        let mut frame = vec![0x80, 96];
        frame.extend(self.sequence.to_be_bytes());
        frame.extend(self.timestamp.wrapping_add(ticks).to_be_bytes());
        frame.extend(self.ssrc.to_be_bytes());
        frame.extend(number.to_be_bytes());
        let encrypted = self
            .cipher
            .encrypt(
                (&nonce(&self.prefix, number)).into(),
                Payload {
                    msg: body,
                    aad: &aad(&self.allocation, &frame),
                },
            )
            .ok()?;
        frame.extend(encrypted);
        self.sequence = self.sequence.wrapping_add(1);
        Some(frame)
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Packet {
    Data(Vec<u8>),
    Control(Control),
}
impl Rx {
    pub fn new(
        secret: &[u8; 32],
        allocation: [u8; 16],
        direction: Direction,
        inner: Ipv4Addr,
    ) -> Self {
        let (key, prefix) = material(secret, &allocation, direction);
        Self {
            cipher: XChaCha20Poly1305::new((&key).into()),
            prefix,
            allocation,
            replay: Replay::default(),
            direction,
            inner,
        }
    }
    pub fn open(&mut self, frame: &[u8]) -> Option<Packet> {
        if !(OVERHEAD + 1..=MAX_FRAME).contains(&frame.len()) || frame[..2] != [0x80, 96] {
            return None;
        }
        let number = u64::from_be_bytes(frame[12..20].try_into().ok()?);
        if !self.replay.permits(number) {
            return None;
        }
        let body = self
            .cipher
            .decrypt(
                (&nonce(&self.prefix, number)).into(),
                Payload {
                    msg: &frame[20..],
                    aad: &aad(&self.allocation, &frame[..20]),
                },
            )
            .ok()?;
        // A structurally bad authenticated payload is not traffic and has no state effect.
        let packet = if body.first()? >> 4 == 4 {
            if !(16..=crate::MTU - 4).contains(&body.len()) {
                return None;
            }
            let offset = self.direction.offset();
            if body.len() < offset {
                return None;
            }
            let ip = [&body[..offset], &self.inner.octets(), &body[offset..]].concat();
            if !crate::ipv4(&ip) {
                return None;
            }
            Packet::Data(ip)
        } else {
            Packet::Control(Control::decode(&body)?)
        };
        self.replay.accept(number);
        Some(packet)
    }
}
#[derive(Default)]
struct Replay {
    high: Option<u64>,
    bits: [u64; 16],
}
impl Replay {
    fn permits(&self, number: u64) -> bool {
        match self.high {
            None => true,
            Some(high) if number > high => true,
            Some(high) => {
                let delta = high - number;
                delta < 1024 && self.bits[delta as usize / 64] & (1 << (delta % 64)) == 0
            }
        }
    }
    fn accept(&mut self, number: u64) {
        match self.high {
            None => {
                self.high = Some(number);
                self.bits[0] = 1;
            }
            Some(high) if number > high => {
                let shift = number - high;
                let old = self.bits;
                self.bits = [0; 16];
                if shift < 1024 {
                    for bit in 0..1024 - shift as usize {
                        if old[bit / 64] & (1 << (bit % 64)) != 0 {
                            let target = bit + shift as usize;
                            self.bits[target / 64] |= 1 << (target % 64);
                        }
                    }
                }
                self.bits[0] |= 1;
                self.high = Some(number);
            }
            Some(high) => {
                let delta = (high - number) as usize;
                self.bits[delta / 64] |= 1 << (delta % 64);
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    PathOpen {
        attempt: u64,
        token: [u8; 16],
    },
    PathChallenge {
        attempt: u64,
        token: [u8; 16],
        challenge: [u8; 16],
    },
    PathResponse {
        attempt: u64,
        token: [u8; 16],
        challenge: [u8; 16],
    },
    PathReady {
        attempt: u64,
        token: [u8; 16],
    },
    Activate {
        activation: u64,
        attempt: u64,
        token: [u8; 16],
    },
    Activated {
        activation: u64,
        attempt: u64,
        token: [u8; 16],
    },
    Ping([u8; 16]),
    Pong([u8; 16]),
}
impl Control {
    pub fn encode(self) -> Vec<u8> {
        let (kind, attempt, token, extra) = match self {
            Self::PathOpen { attempt, token } => (1, Some(attempt), token, None),
            Self::PathChallenge {
                attempt,
                token,
                challenge,
            } => (2, Some(attempt), token, Some(challenge.to_vec())),
            Self::PathResponse {
                attempt,
                token,
                challenge,
            } => (3, Some(attempt), token, Some(challenge.to_vec())),
            Self::PathReady { attempt, token } => (5, Some(attempt), token, None),
            Self::Activate {
                activation,
                attempt,
                token,
            } => (
                7,
                Some(attempt),
                token,
                Some(activation.to_be_bytes().to_vec()),
            ),
            Self::Activated {
                activation,
                attempt,
                token,
            } => (
                8,
                Some(attempt),
                token,
                Some(activation.to_be_bytes().to_vec()),
            ),
            Self::Ping(token) => (9, None, token, None),
            Self::Pong(token) => (10, None, token, None),
        };
        let mut b = vec![kind];
        if kind == 7 || kind == 8 {
            b.extend(extra.as_ref().unwrap());
        }
        if let Some(a) = attempt {
            b.extend(a.to_be_bytes());
        }
        b.extend(token);
        if kind == 2 || kind == 3 {
            b.extend(extra.unwrap());
        }
        b
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        let kind = *b.first()?;
        let length = match kind {
            1 | 5 => 25,
            2 | 3 => 41,
            7 | 8 => 33,
            9 | 10 => 17,
            _ => return None,
        };
        if b.len() != length {
            return None;
        }
        if kind == 9 || kind == 10 {
            let token = b[1..17].try_into().ok()?;
            return Some(if kind == 9 {
                Self::Ping(token)
            } else {
                Self::Pong(token)
            });
        }
        let offset = if kind == 7 || kind == 8 { 9 } else { 1 };
        let attempt = u64::from_be_bytes(b[offset..offset + 8].try_into().ok()?);
        let token = b[offset + 8..offset + 24].try_into().ok()?;
        Some(match kind {
            1 => Self::PathOpen { attempt, token },
            5 => Self::PathReady { attempt, token },
            2 => Self::PathChallenge {
                attempt,
                token,
                challenge: b[25..].try_into().ok()?,
            },
            3 => Self::PathResponse {
                attempt,
                token,
                challenge: b[25..].try_into().ok()?,
            },
            7 => Self::Activate {
                activation: u64::from_be_bytes(b[1..9].try_into().ok()?),
                attempt,
                token,
            },
            8 => Self::Activated {
                activation: u64::from_be_bytes(b[1..9].try_into().ok()?),
                attempt,
                token,
            },
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ip(n: usize, options: bool) -> Vec<u8> {
        let mut b: Vec<_> = (0..n.max(20)).map(|i| i as u8).collect();
        b[0] = if options { 0x46 } else { 0x45 };
        b[2..4].copy_from_slice(&(n as u16).to_be_bytes());
        b[12..16].copy_from_slice(&[10, 0, 0, 2]);
        b[16..20].copy_from_slice(&[10, 0, 0, 2]);
        b.truncate(n);
        b
    }
    fn pair(direction: Direction) -> (Tx, Rx) {
        let inner = Ipv4Addr::new(10, 0, 0, 2);
        (
            Tx::new(&[7; 32], [9; 16], direction, inner),
            Rx::new(&[7; 32], [9; 16], direction, inner),
        )
    }
    #[test]
    fn independent_libsodium_vectors_both_directions() {
        let vectors: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/udp-v2.json")).unwrap();
        let decode = |s: &str| {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        for (index, direction) in [Direction::Upstream, Direction::Downstream]
            .into_iter()
            .enumerate()
        {
            let v = &vectors[index];
            let (mut tx, mut rx) = pair(direction);
            tx.sequence = 0x0102;
            tx.timestamp = 0x03040506;
            tx.ssrc = 0x0708090a;
            let ip = decode(v["ip"].as_str().unwrap());
            let expected = decode(v["frame"].as_str().unwrap());
            let (key, prefix) = material(&[7; 32], &[9; 16], direction);
            assert_eq!(key.as_slice(), decode(v["key"].as_str().unwrap()));
            assert_eq!(prefix.as_slice(), decode(v["prefix"].as_str().unwrap()));
            assert_eq!(tx.data(&ip, 0).unwrap(), expected);
            assert_eq!(rx.open(&expected), Some(Packet::Data(ip)));
        }
    }
    #[test]
    fn exact_compression_options_fragments_and_bounds() {
        for direction in [Direction::Upstream, Direction::Downstream] {
            let (mut tx, mut rx) = pair(direction);
            for (n, opt) in [(20, false), (24, true), (1200, true)] {
                let packet = ip(n, opt);
                let frame = tx.data(&packet, 1).unwrap();
                assert_eq!(frame.len(), n + 32);
                assert_eq!(rx.open(&frame), Some(Packet::Data(packet)));
            }
            for n in [19, 1201] {
                assert!(tx.data(&ip(n, false), 0).is_none());
            }
            let mut wrong = ip(60, false);
            wrong[direction.offset()] = 11;
            assert!(tx.data(&wrong, 0).is_none());
            for first in [0x44, 0x4f, 0x60] {
                let mut b = ip(20, false);
                b[0] = first;
                assert!(tx.data(&b, 0).is_none());
            }
        }
    }
    #[test]
    fn authenticate_every_byte_before_replay_commit() {
        let (mut tx, mut rx) = pair(Direction::Upstream);
        let p = tx.data(&ip(100, false), 0).unwrap();
        for i in 0..p.len() {
            let mut q = p.clone();
            q[i] ^= 1;
            assert!(rx.open(&q).is_none(), "byte {i}");
        }
        for n in 0..p.len() {
            assert!(rx.open(&p[..n]).is_none());
        }
        assert!(
            Rx::new(&[8; 32], [9; 16], Direction::Upstream, Ipv4Addr::LOCALHOST)
                .open(&p)
                .is_none()
        );
        assert!(
            Rx::new(&[7; 32], [8; 16], Direction::Upstream, Ipv4Addr::LOCALHOST)
                .open(&p)
                .is_none()
        );
        assert!(Rx::new(
            &[7; 32],
            [9; 16],
            Direction::Downstream,
            Ipv4Addr::LOCALHOST
        )
        .open(&p)
        .is_none());
        assert!(rx.open(&p).is_some());
        assert!(rx.open(&p).is_none());
    }
    #[test]
    fn replay_boundary_reordering_and_large_jump() {
        let (mut tx, mut rx) = pair(Direction::Upstream);
        let p: Vec<_> = (0..1030)
            .map(|_| tx.data(&ip(20, false), 0).unwrap())
            .collect();
        assert!(rx.open(&p[1024]).is_some());
        assert!(rx.open(&p[0]).is_none());
        assert!(rx.open(&p[1]).is_some());
        assert!(rx.open(&p[100]).is_some());
        assert!(rx.open(&p[100]).is_none());
        tx.next = Some(5000);
        let q = tx.data(&ip(20, false), 0).unwrap();
        assert!(rx.open(&q).is_some());
        assert!(rx.open(&p[1029]).is_none());
    }
    #[test]
    fn exhaustion_does_not_wrap_and_rtp_rollover_is_independent() {
        let (mut tx, mut rx) = pair(Direction::Upstream);
        tx.sequence = u16::MAX;
        tx.timestamp = u32::MAX;
        let a = tx.data(&ip(20, false), 0).unwrap();
        let b = tx.data(&ip(20, false), 1).unwrap();
        assert_eq!(&b[2..8], &[0; 6]);
        assert!(rx.open(&b).is_some());
        assert!(rx.open(&a).is_some());
        tx.next = Some(u64::MAX);
        assert!(tx.data(&ip(20, false), 0).is_some());
        assert!(tx.data(&ip(20, false), 0).is_none());
    }
    #[test]
    fn control_exact_encodings_and_reserved_nibbles() {
        let controls = [
            Control::PathOpen {
                attempt: 2,
                token: [3; 16],
            },
            Control::PathChallenge {
                attempt: 2,
                token: [3; 16],
                challenge: [4; 16],
            },
            Control::PathResponse {
                attempt: 2,
                token: [3; 16],
                challenge: [4; 16],
            },
            Control::PathReady {
                attempt: 2,
                token: [3; 16],
            },
            Control::Activate {
                activation: 1,
                attempt: 2,
                token: [3; 16],
            },
            Control::Activated {
                activation: 1,
                attempt: 2,
                token: [3; 16],
            },
            Control::Ping([5; 16]),
            Control::Pong([5; 16]),
        ];
        let (mut tx, mut rx) = pair(Direction::Downstream);
        for c in controls {
            let mut b = c.encode();
            assert_eq!(Control::decode(&b), Some(c));
            b.push(0);
            assert!(Control::decode(&b).is_none());
            let f = tx.control(c, 0).unwrap();
            assert_eq!(rx.open(&f), Some(Packet::Control(c)));
        }
        for kind in [4, 6, 0x60] {
            assert!(Control::decode(&[kind; 25]).is_none());
        }
    }
}
