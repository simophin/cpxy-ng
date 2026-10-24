//! Experimental RTP-framed IPv4 tunnel; not SRTP or encoded media.
//!
//! Handshake-free raw XChaCha20 with 16 random nonce bytes per packet. No tag,
//! replay protection, sender authenticity or forward secrecy on data by design.
//! V4 adds authenticated control, session registration and per-port keys.
use chacha20::{
    cipher::{KeyIvInit, StreamCipher},
    XChaCha20,
};
use hkdf::Hkdf;
use rand::{rngs::OsRng, RngCore};
use sha2::Sha256;

pub mod control;
pub mod state;

pub const MTU: usize = 1200;

fn ipv4(p: &[u8]) -> bool {
    p.len() >= 20
        && p.len() <= MTU
        && p[0] >> 4 == 4
        && (p[0] & 15) >= 5
        && (p[0] as usize & 15) * 4 <= p.len()
        && u16::from_be_bytes([p[2], p[3]]) as usize == p.len()
}

/// Random nonce bytes carried after the RTP header.
pub const RANDOM: usize = 16;
pub const OVERHEAD: usize = 12 + RANDOM;
const C2S: &[u8] = b"rtp-l3-poc/xchacha20/v3/client-to-server";
const S2C: &[u8] = b"rtp-l3-poc/xchacha20/v3/server-to-client";

/// Returns (transmit, receive) keys for this side.
pub fn keys(psk: &[u8; 32], client: bool) -> ([u8; 32], [u8; 32]) {
    let h = Hkdf::<Sha256>::new(None, psk);
    let (mut a, mut b) = ([0; 32], [0; 32]);
    h.expand(C2S, &mut a).unwrap();
    h.expand(S2C, &mut b).unwrap();
    if client {
        (a, b)
    } else {
        (b, a)
    }
}
/// Nonce = RTP header bytes 2..10 (sequence, timestamp, SSRC high half)
/// followed by the packet's 16 random bytes; uniqueness rests on the latter.
fn xor(key: &[u8; 32], p: &mut [u8]) {
    let mut nonce = [0u8; 24];
    nonce[..8].copy_from_slice(&p[2..10]);
    nonce[8..].copy_from_slice(&p[12..OVERHEAD]);
    XChaCha20::new(key.into(), (&nonce).into()).apply_keystream(&mut p[OVERHEAD..]);
}
pub struct Tx {
    key: [u8; 32],
    ssrc: u32,
    seq: u16,
    offset: u32,
}
impl Tx {
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            key,
            ssrc: OsRng.next_u32(),
            seq: OsRng.next_u32() as u16,
            offset: OsRng.next_u32(),
        }
    }
    /// `ticks` is monotonic elapsed 90 kHz time; framing only.
    pub fn seal(&mut self, ip: &[u8], ticks: u32) -> Option<Vec<u8>> {
        let mut random = [0; RANDOM];
        OsRng.fill_bytes(&mut random);
        self.seal_with(ip, ticks, &random)
    }
    fn seal_with(&mut self, ip: &[u8], ticks: u32, random: &[u8; RANDOM]) -> Option<Vec<u8>> {
        if !ipv4(ip) {
            return None;
        }
        Some(self.seal_body(ip, ticks, random))
    }
    fn seal_body(&mut self, body: &[u8], ticks: u32, random: &[u8; RANDOM]) -> Vec<u8> {
        let mut p = Vec::with_capacity(OVERHEAD + body.len());
        p.extend([0x80, 96]);
        p.extend(self.seq.to_be_bytes());
        p.extend(self.offset.wrapping_add(ticks).to_be_bytes());
        p.extend(self.ssrc.to_be_bytes());
        p.extend(random);
        p.extend(body);
        xor(&self.key, &mut p);
        self.seq = self.seq.wrapping_add(1);
        p
    }
}
/// Stateless: the nonce prefix is read from each received header, so
/// rollover, loss, reordering and peer restarts never affect decryption.
pub fn open(key: &[u8; 32], p: &[u8]) -> Option<Vec<u8>> {
    if p.len() < OVERHEAD + 20 || p.len() > OVERHEAD + MTU || p[..2] != [0x80, 96] {
        return None;
    }
    let mut q = p.to_vec();
    xor(key, &mut q);
    let ip = q.split_off(OVERHEAD);
    ipv4(&ip).then_some(ip)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ip(n: usize) -> Vec<u8> {
        let mut p: Vec<u8> = (0..n).map(|i| i as u8).collect();
        p[0] = 0x45;
        p[2..4].copy_from_slice(&(n as u16).to_be_bytes());
        p
    }
    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
    fn pair(psk: u8) -> ([u8; 32], [u8; 32]) {
        // (client transmit key, matching server receive key)
        (keys(&[psk; 32], true).0, keys(&[psk; 32], false).1)
    }
    #[test]
    fn libsodium_counter_zero_vector() {
        // libsodium crypto_stream_xchacha20_xor (counter 0) of ip(40) under key
        // 0x80..0x9f, nonce 0102030405060708 || 0x40..0x4f, generated independently.
        let key: [u8; 32] = core::array::from_fn(|i| 0x80 + i as u8);
        let random: [u8; RANDOM] = core::array::from_fn(|i| 0x40 + i as u8);
        let mut t = Tx {
            key,
            ssrc: 0x0708090a,
            seq: 0x0102,
            offset: 0x03040506,
        };
        let p = t.seal_with(&ip(40), 0, &random).unwrap();
        assert_eq!(p[..12], hex("80600102030405060708090a"));
        assert_eq!(&p[12..OVERHEAD], &random);
        assert_eq!(
            p[OVERHEAD..],
            hex("b5ced925dc386cc4263e486a5d9c82be477b0ebd57b46971c145a8f7e0aefa7cf85d68858989de42")
        );
        assert_eq!(open(&key, &p), Some(ip(40)));
    }
    #[test]
    fn draft_xchacha_vector() {
        // draft-arciszewski-xchacha-03 A.3.2 keystream starts at block 1.
        let key: [u8; 32] = core::array::from_fn(|i| 0x80 + i as u8);
        let nonce = hex("404142434445464748494a4b4c4d4e4f5051525354555658");
        let mut p = vec![0u8; OVERHEAD + 96];
        p[2..10].copy_from_slice(&nonce[..8]);
        p[12..OVERHEAD].copy_from_slice(&nonce[8..]);
        xor(&key, &mut p);
        assert_eq!(
            p[OVERHEAD + 64..],
            hex("29624b4b1b140ace53740e405b2168540fd7d630c1f536fecd722fc3cddba7f4")
        );
    }
    #[test]
    fn directional_roundtrip_without_setup() {
        let (ctx, crx) = keys(&[7; 32], true);
        let (stx, srx) = keys(&[7; 32], false);
        assert_eq!((ctx, crx), (srx, stx));
        assert_ne!(ctx, crx);
        assert_ne!(ctx, keys(&[8; 32], true).0);
        let (mut c, mut s) = (Tx::new(ctx), Tx::new(stx));
        let p = c.seal(&ip(MTU), 0).unwrap();
        assert_eq!(p.len(), MTU + 28);
        assert_eq!(open(&srx, &p), Some(ip(MTU)));
        assert!(open(&crx, &p).is_none());
        let q = s.seal(&ip(20), 0).unwrap();
        assert_eq!(open(&crx, &q), Some(ip(20)));
        assert!(open(&srx, &q).is_none());
    }
    #[test]
    fn framing_rollover_loss_reorder_replay_and_restart() {
        let (tx, rx) = pair(9);
        let mut t = Tx::new(tx);
        t.seq = 65534;
        t.offset = u32::MAX;
        let ps: Vec<_> = (0..4).map(|i| t.seal(&ip(60), i).unwrap()).collect();
        assert_eq!(ps[2][2..4], [0, 0]);
        assert_eq!(ps[1][4..8], [0, 0, 0, 0]);
        for i in [3, 0, 2, 2, 3] {
            assert_eq!(open(&rx, &ps[i]), Some(ip(60)));
        }
        // A restarted sender has new SSRC/sequence but remains decryptable.
        let mut restarted = Tx::new(tx);
        let r = restarted.seal(&ip(60), 0).unwrap();
        assert_eq!(open(&rx, &r), Some(ip(60)));
    }
    #[test]
    fn random_part_fresh_even_with_identical_headers() {
        let mut t = Tx::new([1; 32]);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10000 {
            t.seq = 5; // identical header prefix: uniqueness must come from RNG
            let p = t.seal(&ip(20), 0).unwrap();
            assert!(seen.insert(p[12..OVERHEAD].to_vec()));
        }
    }
    #[test]
    fn length_header_and_tamper_behaviour() {
        let (tx, rx) = pair(3);
        let mut t = Tx::new(tx);
        assert!(t.seal(&ip(MTU + 1), 0).is_none());
        assert!(t.seal(&ip(19), 0).is_none());
        let p = t.seal(&ip(100), 0).unwrap();
        assert!(open(&rx, &p[..p.len() - 1]).is_none());
        assert!(open(&rx, &p[..OVERHEAD + 19]).is_none());
        let mut long = p.clone();
        long.resize(OVERHEAD + MTU + 1, 0);
        assert!(open(&rx, &long).is_none());
        let mut bad = p.clone();
        bad[1] = 97;
        assert!(open(&rx, &bad).is_none());
        // No tag: a flipped payload bit decrypts to a modified packet.
        let mut flipped = p.clone();
        flipped[OVERHEAD + 50] ^= 1;
        assert_ne!(open(&rx, &flipped).unwrap(), ip(100));
        // Header bytes feed the nonce: changing the sequence garbles the
        // whole packet (almost always failing the IPv4 check).
        let mut seq = p.clone();
        seq[3] ^= 1;
        assert_ne!(open(&rx, &seq), Some(ip(100)));
        // SSRC low half is framing only.
        let mut ssrc = p.clone();
        ssrc[11] ^= 1;
        assert_eq!(open(&rx, &ssrc), Some(ip(100)));
    }
}
