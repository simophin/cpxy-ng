//! Experimental RTP-framed encrypted IPv4; not SRTP or encoded media.
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::{Duration, Instant};
pub const MTU: usize = 1200;
pub const HELLO: u8 = 1;
pub const CHALLENGE: u8 = 2;
pub const FINISH: u8 = 3;
pub const READY: u8 = 4;
const DOMAIN: &[u8] = b"rtp-l3-poc/control/v1";
type Hmac256 = Hmac<Sha256>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Control {
    pub kind: u8,
    pub c: [u8; 32],
    pub s: [u8; 32],
}
impl Control {
    pub fn encode(&self, key: &[u8; 32]) -> Vec<u8> {
        let mut out = b"R3P1".to_vec();
        out.push(self.kind);
        out.extend(self.c);
        if self.kind != HELLO {
            out.extend(self.s);
        }
        let mut mac = <Hmac256 as Mac>::new_from_slice(key).unwrap();
        mac.update(DOMAIN);
        mac.update(&out);
        out.extend(mac.finalize().into_bytes());
        out
    }
    pub fn decode(b: &[u8], key: &[u8; 32]) -> Option<Self> {
        if b.len() < 5 || &b[..4] != b"R3P1" || !(1..=4).contains(&b[4]) {
            return None;
        }
        let n = if b[4] == HELLO { 37 } else { 69 };
        if b.len() != n + 32 {
            return None;
        }
        let mut mac = <Hmac256 as Mac>::new_from_slice(key).unwrap();
        mac.update(DOMAIN);
        mac.update(&b[..n]);
        mac.verify_slice(&b[n..]).ok()?;
        let mut c = [0; 32];
        c.copy_from_slice(&b[5..37]);
        let mut s = [0; 32];
        if n == 69 {
            s.copy_from_slice(&b[37..69]);
        }
        Some(Self { kind: b[4], c, s })
    }
    pub fn with_kind(&self, kind: u8) -> Self {
        Self {
            kind,
            ..self.clone()
        }
    }
}

pub struct ServerHandshake {
    pending: Option<(Control, String, Instant)>,
    installed: Option<(Control, String)>,
}
impl Default for ServerHandshake {
    fn default() -> Self {
        Self {
            pending: None,
            installed: None,
        }
    }
}
impl ServerHandshake {
    /// Only decoded/authenticated control messages may enter here.
    /// `install` is true exactly once for each new transcript.
    pub fn receive(&mut self, m: Control, peer: String, now: Instant) -> Option<(Control, bool)> {
        if m.kind == HELLO {
            if let Some((old, p, t)) = &self.pending {
                if old.c == m.c && *p == peer && now.duration_since(*t) < Duration::from_secs(10) {
                    return Some((old.with_kind(CHALLENGE), false));
                }
            }
            let challenge = Control {
                kind: CHALLENGE,
                c: m.c,
                s: rand::random(),
            };
            self.pending = Some((challenge.clone(), peer, now));
            return Some((challenge, false));
        }
        if m.kind == FINISH {
            if let Some((old, p)) = &self.installed {
                if old.c == m.c && old.s == m.s && *p == peer {
                    return Some((old.with_kind(READY), false));
                }
            }
            if let Some((old, p, t)) = &self.pending {
                if old.c == m.c
                    && old.s == m.s
                    && *p == peer
                    && now.duration_since(*t) < Duration::from_secs(10)
                {
                    self.installed = Some((m.clone(), peer));
                    self.pending = None;
                    return Some((m.with_kind(READY), true));
                }
            }
        }
        None
    }
}

pub struct Tx {
    cipher: ChaCha20Poly1305,
    ssrc: [u8; 4],
    index: u64,
}
pub struct Rx {
    cipher: ChaCha20Poly1305,
    ssrc: [u8; 4],
    highest: Option<u64>,
    bitmap: u128,
}
pub struct Session {
    pub tx: Tx,
    pub rx: Rx,
}
fn material(psk: &[u8; 32], c: &[u8; 32], s: &[u8; 32], label: &[u8]) -> ([u8; 32], [u8; 4]) {
    let mut salt = [0; 64];
    salt[..32].copy_from_slice(c);
    salt[32..].copy_from_slice(s);
    let h = Hkdf::<Sha256>::new(Some(&salt), psk);
    let mut out = [0; 36];
    h.expand(label, &mut out).unwrap();
    (out[..32].try_into().unwrap(), out[32..].try_into().unwrap())
}
impl Session {
    pub fn new(psk: &[u8; 32], c: &[u8; 32], s: &[u8; 32], client: bool) -> Self {
        let a = material(psk, c, s, b"rtp-l3-poc/traffic/v1/client-to-server");
        let b = material(psk, c, s, b"rtp-l3-poc/traffic/v1/server-to-client");
        let (t, r) = if client { (a, b) } else { (b, a) };
        Self {
            tx: Tx {
                cipher: ChaCha20Poly1305::new((&t.0).into()),
                ssrc: t.1,
                index: 0,
            },
            rx: Rx {
                cipher: ChaCha20Poly1305::new((&r.0).into()),
                ssrc: r.1,
                highest: None,
                bitmap: 0,
            },
        }
    }
}
fn nonce(index: u64) -> [u8; 12] {
    let mut n = [0; 12];
    n[4..].copy_from_slice(&index.to_be_bytes());
    n
}
fn ipv4(p: &[u8]) -> bool {
    p.len() >= 20
        && p.len() <= MTU
        && p[0] >> 4 == 4
        && (p[0] & 15) >= 5
        && (p[0] as usize & 15) * 4 <= p.len()
        && u16::from_be_bytes([p[2], p[3]]) as usize == p.len()
}
impl Tx {
    pub fn seal(&mut self, ip: &[u8], timestamp: u32) -> Option<Vec<u8>> {
        if !ipv4(ip) || self.index >= 1 << 48 {
            return None;
        }
        let mut h = vec![0x80, 96];
        h.extend((self.index as u16).to_be_bytes());
        h.extend(timestamp.to_be_bytes());
        h.extend(self.ssrc);
        let n = nonce(self.index);
        let body = self
            .cipher
            .encrypt(Nonce::from_slice(&n), Payload { msg: ip, aad: &h })
            .ok()?;
        self.index += 1;
        h.extend(body);
        Some(h)
    }
}
impl Rx {
    pub fn open(&mut self, p: &[u8]) -> Option<Vec<u8>> {
        if p.len() < 48 || p.len() > MTU + 28 || p[..2] != [0x80, 96] || p[8..12] != self.ssrc {
            return None;
        }
        let seq = u16::from_be_bytes([p[2], p[3]]) as u64;
        let index = match self.highest {
            None => seq,
            Some(h) => {
                let base = (h & !65535) | seq;
                if base + 32768 < h {
                    base.checked_add(65536)?
                } else if base > h + 32768 {
                    base.checked_sub(65536)?
                } else {
                    base
                }
            }
        };
        if index >= 1 << 48 {
            return None;
        }
        if let Some(h) = self.highest {
            if index <= h {
                let d = h - index;
                if d >= 128 || self.bitmap & (1u128 << d) != 0 {
                    return None;
                }
            } else if index - h >= 32768 {
                return None;
            }
        }
        let n = nonce(index);
        let ip = self
            .cipher
            .decrypt(
                Nonce::from_slice(&n),
                Payload {
                    msg: &p[12..],
                    aad: &p[..12],
                },
            )
            .ok()?;
        if !ipv4(&ip) {
            return None;
        }
        match self.highest {
            None => {
                self.highest = Some(index);
                self.bitmap = 1;
            }
            Some(h) if index > h => {
                let d = index - h;
                self.bitmap = if d >= 128 { 1 } else { (self.bitmap << d) | 1 };
                self.highest = Some(index);
            }
            Some(h) => self.bitmap |= 1u128 << (h - index),
        }
        Some(ip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ip() -> Vec<u8> {
        let mut p = vec![0; 20];
        p[0] = 0x45;
        p[3] = 20;
        p
    }
    fn pair() -> (Session, Session) {
        (
            Session::new(&[1; 32], &[2; 32], &[3; 32], true),
            Session::new(&[1; 32], &[2; 32], &[3; 32], false),
        )
    }
    #[test]
    fn roundtrip_tamper_replay() {
        let (mut a, mut b) = pair();
        let p = a.tx.seal(&ip(), 5).unwrap();
        for i in 0..p.len() {
            let mut q = p.clone();
            q[i] ^= 1;
            assert!(b.rx.open(&q).is_none());
            assert_eq!(b.rx.highest, None);
        }
        assert_eq!(b.rx.open(&p), Some(ip()));
        assert!(b.rx.open(&p).is_none());
        let q = b.tx.seal(&ip(), 7).unwrap();
        assert_eq!(a.rx.open(&q), Some(ip()));
        assert!(a.rx.open(&p).is_none());
    }
    #[test]
    fn rollover_reorder_and_invalid_future() {
        let (mut a, mut b) = pair();
        a.tx.index = 65534;
        b.rx.highest = Some(65533);
        b.rx.bitmap = 1;
        let p = a.tx.seal(&ip(), 1).unwrap();
        let q = a.tx.seal(&ip(), 2).unwrap();
        let r = a.tx.seal(&ip(), 3).unwrap();
        assert!(b.rx.open(&r).is_some());
        assert!(b.rx.open(&p).is_some());
        assert!(b.rx.open(&q).is_some());
        assert_eq!(b.rx.highest, Some(65536));
        let mut bad = a.tx.seal(&ip(), 4).unwrap();
        bad[2..4].copy_from_slice(&30000u16.to_be_bytes());
        assert!(b.rx.open(&bad).is_none());
        assert_eq!(b.rx.highest, Some(65536));
    }
    #[test]
    fn replay_window_and_mtu() {
        let (mut a, mut b) = pair();
        let p = a.tx.seal(&ip(), 0).unwrap();
        for _ in 0..130 {
            let q = a.tx.seal(&ip(), 0).unwrap();
            assert!(b.rx.open(&q).is_some());
        }
        assert!(b.rx.open(&p).is_none());
        assert!(a.tx.seal(&vec![0; 1201], 0).is_none());
        assert!(b.rx.open(&[0; 12]).is_none());
    }
    #[test]
    fn control_authentication() {
        let m = Control {
            kind: HELLO,
            c: [3; 32],
            s: [0; 32],
        };
        let p = m.encode(&[4; 32]);
        assert_eq!(Control::decode(&p, &[4; 32]), Some(m));
        for i in 0..p.len() {
            let mut q = p.clone();
            q[i] ^= 1;
            assert!(Control::decode(&q, &[4; 32]).is_none());
        }
    }
    #[test]
    fn handshake_retries_restart_and_peer() {
        let mut h = ServerHandshake::default();
        let now = Instant::now();
        let m = Control {
            kind: HELLO,
            c: [3; 32],
            s: [0; 32],
        };
        let (q, install) = h.receive(m.clone(), "a".into(), now).unwrap();
        assert!(!install);
        assert_eq!(h.receive(m.clone(), "a".into(), now).unwrap().0, q);
        assert!(h.receive(q.with_kind(FINISH), "b".into(), now).is_none());
        assert!(h.receive(q.with_kind(FINISH), "a".into(), now).unwrap().1);
        assert!(!h.receive(q.with_kind(FINISH), "a".into(), now).unwrap().1);
        let mut restart = ServerHandshake::default();
        let fresh = restart.receive(m, "a".into(), now).unwrap().0;
        assert_ne!(fresh.s, q.s);
        assert!(restart
            .receive(q.with_kind(FINISH), "a".into(), now)
            .is_none());
    }
    #[test]
    fn stale_data_and_nonce_uniqueness() {
        let (mut a, mut b) = pair();
        let p = a.tx.seal(&ip(), 0).unwrap();
        let q = a.tx.seal(&ip(), 0).unwrap();
        assert_ne!(p, q);
        assert!(b.rx.open(&p).is_some());
        let mut fresh = Session::new(&[1; 32], &[2; 32], &[4; 32], false);
        assert!(fresh.rx.open(&q).is_none());
        a.tx.index = (1 << 48) - 1;
        assert!(a.tx.seal(&ip(), 0).is_some());
        assert!(a.tx.seal(&ip(), 0).is_none());
    }
    #[test]
    fn timeout() {
        let mut h = ServerHandshake::default();
        let now = Instant::now();
        let m = Control {
            kind: HELLO,
            c: [3; 32],
            s: [0; 32],
        };
        let q = h.receive(m, "a".into(), now).unwrap().0;
        assert!(h
            .receive(
                q.with_kind(FINISH),
                "a".into(),
                now + Duration::from_secs(11)
            )
            .is_none());
    }
}
