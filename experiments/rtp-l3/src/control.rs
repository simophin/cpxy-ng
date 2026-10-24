//! Authenticated control codec; data encryption carries no MAC.
use crate::{ipv4, keys, xor, Tx, MTU, OVERHEAD, RANDOM};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, Rng, RngCore};
use sha2::Sha256;
use std::net::Ipv4Addr;

const TAG: usize = 16;
pub const MAX_CONTROL_IPV4: usize = 260;

#[derive(Clone, PartialEq, Eq)]
pub struct PortKeys {
    pub tx_data: [u8; 32],
    pub rx_data: [u8; 32],
    pub tx_control: [u8; 32],
    pub rx_control: [u8; 32],
}
fn derive(h: &Hkdf<Sha256>, label: &[u8]) -> [u8; 32] {
    let mut out = [0; 32];
    h.expand(label, &mut out).unwrap();
    out
}
impl PortKeys {
    pub fn base(psk: &[u8; 32], client: bool) -> Self {
        let (tx_data, rx_data) = keys(psk, client);
        let h = Hkdf::<Sha256>::new(None, psk);
        let a = derive(&h, b"rtp-l3-poc/control/client-to-server");
        let b = derive(&h, b"rtp-l3-poc/control/server-to-client");
        let (tx_control, rx_control) = if client { (a, b) } else { (b, a) };
        Self {
            tx_data,
            rx_data,
            tx_control,
            rx_control,
        }
    }
    /// A fresh secret is required for every bound rotation port, including reuse
    /// of a numeric port. Neither secrets nor derived keys implement Debug.
    pub fn rotation(psk: &[u8; 32], secret: &[u8; 32], client: bool) -> Self {
        let salt = derive(&Hkdf::<Sha256>::new(None, psk), b"rtp-l3-poc/port-salt");
        let h = Hkdf::<Sha256>::new(Some(&salt), secret);
        let a = derive(&h, b"rtp-l3-poc/port/data/client-to-server");
        let b = derive(&h, b"rtp-l3-poc/port/data/server-to-client");
        let c = derive(&h, b"rtp-l3-poc/port/control/client-to-server");
        let d = derive(&h, b"rtp-l3-poc/port/control/server-to-client");
        let (tx_data, rx_data, tx_control, rx_control) =
            if client { (a, b, c, d) } else { (b, a, d, c) };
        Self {
            tx_data,
            rx_data,
            tx_control,
            rx_control,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Control {
    Ping {
        session: u64,
        inner: Ipv4Addr,
        seq: u32,
        instance: u64,
        epoch: u32,
    },
    State {
        session: u64,
        echo: u32,
        instance: u64,
        epoch: u32,
        port: u16,
        secret: [u8; 32],
        base: u16,
        grace: u16,
    },
}
impl Control {
    fn body(&self) -> Vec<u8> {
        // Reserved byte must be zero; there is one experimental format.
        let mut b = vec![0, 0, 0, 0];
        match self {
            Self::Ping {
                session,
                inner,
                seq,
                instance,
                epoch,
            } => {
                b[0] = 1;
                b.extend(session.to_be_bytes());
                b.extend(inner.octets());
                b.extend(seq.to_be_bytes());
                b.extend(instance.to_be_bytes());
                b.extend(epoch.to_be_bytes());
            }
            Self::State {
                session,
                echo,
                instance,
                epoch,
                port,
                secret,
                base,
                grace,
            } => {
                b[0] = 2;
                b.extend(session.to_be_bytes());
                b.extend(echo.to_be_bytes());
                b.extend(instance.to_be_bytes());
                b.extend(epoch.to_be_bytes());
                b.extend(port.to_be_bytes());
                b.extend(secret);
                b.extend(base.to_be_bytes());
                b.extend(grace.to_be_bytes());
            }
        }
        let len = b.len() as u16;
        b[2..4].copy_from_slice(&len.to_be_bytes());
        b
    }
    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 4 || b[1] != 0 {
            return None;
        }
        let len = u16::from_be_bytes(b[2..4].try_into().ok()?) as usize;
        if len < 4 || len > b.len() || b[len..].iter().any(|x| *x != 0) {
            return None;
        }
        let b = &b[..len];
        let u16_at = |i| u16::from_be_bytes(b[i..i + 2].try_into().unwrap());
        let u32_at = |i| u32::from_be_bytes(b[i..i + 4].try_into().unwrap());
        let u64_at = |i| u64::from_be_bytes(b[i..i + 8].try_into().unwrap());
        match (b[0], len) {
            (1, 32) => Some(Self::Ping {
                session: u64_at(4),
                inner: Ipv4Addr::from(u32_at(12)),
                seq: u32_at(16),
                instance: u64_at(20),
                epoch: u32_at(28),
            }),
            (2, 66) => Some(Self::State {
                session: u64_at(4),
                echo: u32_at(12),
                instance: u64_at(16),
                epoch: u32_at(24),
                port: u16_at(28),
                secret: b[30..62].try_into().ok()?,
                base: u16_at(62),
                grace: u16_at(64),
            }),
            _ => None,
        }
    }
}

/// Control requires a valid MAC; data remains unauthenticated.
#[derive(Clone, PartialEq, Eq)]
pub enum Packet {
    Data(Vec<u8>),
    Control(Control),
}

impl Tx {
    pub fn seal_control(
        &mut self,
        key: &[u8; 32],
        msg: &Control,
        ticks: u32,
        max_ipv4: usize,
    ) -> Option<Vec<u8>> {
        let mut body = msg.body();
        let min = 28 + OVERHEAD + body.len() + TAG;
        if max_ipv4 < min || max_ipv4 > MAX_CONTROL_IPV4 {
            return None;
        }
        let target = OsRng.gen_range(min..=max_ipv4);
        body.resize(target - 28 - OVERHEAD - TAG, 0);
        let mut random = [0; RANDOM];
        OsRng.fill_bytes(&mut random);
        let mut p = self.seal_body(&body, ticks, &random);
        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        mac.update(&p);
        p.extend_from_slice(&mac.finalize().into_bytes()[..TAG]);
        Some(p)
    }
}

pub fn open_packet(data_key: &[u8; 32], control_key: &[u8; 32], p: &[u8]) -> Option<Packet> {
    if p.len() < OVERHEAD + 1 || p.len() > OVERHEAD + MTU || p[..2] != [0x80, 96] {
        return None;
    }
    let mut q = p.to_vec();
    xor(data_key, &mut q);
    match q[OVERHEAD] {
        0x40..=0x4f => {
            let ip = q.split_off(OVERHEAD);
            ipv4(&ip).then_some(Packet::Data(ip))
        }
        1..=15 => {
            if p.len() < OVERHEAD + 4 + TAG || p.len() + 28 > MAX_CONTROL_IPV4 {
                return None;
            }
            let end = p.len() - TAG;
            let mut mac = Hmac::<Sha256>::new_from_slice(control_key).unwrap();
            mac.update(&p[..end]);
            mac.verify_truncated_left(&p[end..]).ok()?;
            Control::parse(&q[OVERHEAD..end]).map(Packet::Control)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ping() -> Control {
        Control::Ping {
            session: 42,
            inner: Ipv4Addr::new(10, 0, 0, 2),
            seq: 7,
            instance: 9,
            epoch: 3,
        }
    }
    fn state() -> Control {
        Control::State {
            session: 42,
            echo: 7,
            instance: 9,
            epoch: 3,
            port: 4001,
            secret: [88; 32],
            base: 4000,
            grace: 60,
        }
    }
    #[test]
    fn codec_roundtrip_padding_and_every_single_bit() {
        for rotation in [false, true] {
            let c = if rotation {
                PortKeys::rotation(&[7; 32], &[8; 32], true)
            } else {
                PortKeys::base(&[7; 32], true)
            };
            let s = if rotation {
                PortKeys::rotation(&[7; 32], &[8; 32], false)
            } else {
                PortKeys::base(&[7; 32], false)
            };
            for msg in [ping(), state()] {
                let (sender, receiver) = if matches!(msg, Control::Ping { .. }) {
                    (&c, &s)
                } else {
                    (&s, &c)
                };
                let mut tx = Tx::new(sender.tx_data);
                for max in [28 + OVERHEAD + msg.body().len() + TAG, MAX_CONTROL_IPV4] {
                    let p = tx.seal_control(&sender.tx_control, &msg, 0, max).unwrap();
                    assert!(p.len() + 28 <= max);
                    assert!(p.len() >= OVERHEAD + msg.body().len() + TAG);
                    assert!(
                        open_packet(&receiver.rx_data, &receiver.rx_control, &p)
                            == Some(Packet::Control(msg.clone()))
                    );
                    for i in 0..p.len() * 8 {
                        let mut corrupt = p.clone();
                        corrupt[i / 8] ^= 1 << (i % 8);
                        assert!(
                            open_packet(&receiver.rx_data, &receiver.rx_control, &corrupt)
                                .is_none(),
                            "bit {i}"
                        );
                    }
                    for len in 0..p.len() {
                        assert!(
                            open_packet(&receiver.rx_data, &receiver.rx_control, &p[..len])
                                .is_none()
                        );
                    }
                }
                assert!(tx.seal_control(&sender.tx_control, &msg, 0, 1).is_none());
                assert!(tx.seal_control(&sender.tx_control, &msg, 0, 261).is_none());
            }
        }
    }
    #[test]
    fn keys_separate_ports_directions_purposes_and_psks() {
        let c = PortKeys::rotation(&[1; 32], &[2; 32], true);
        let s = PortKeys::rotation(&[1; 32], &[2; 32], false);
        assert!(c.tx_data == s.rx_data && c.rx_data == s.tx_data);
        assert!(c.tx_control == s.rx_control && c.rx_control == s.tx_control);
        let mut all = std::collections::HashSet::new();
        for k in [
            c.clone(),
            PortKeys::base(&[1; 32], true),
            PortKeys::rotation(&[1; 32], &[3; 32], true),
            PortKeys::rotation(&[4; 32], &[2; 32], true),
        ] {
            for key in [k.tx_data, k.rx_data, k.tx_control, k.rx_control] {
                assert!(all.insert(key));
            }
        }
        let wrong = PortKeys::rotation(&[1; 32], &[3; 32], false);
        let p = Tx::new(c.tx_data)
            .seal_control(&c.tx_control, &ping(), 0, 260)
            .unwrap();
        assert!(open_packet(&wrong.rx_data, &wrong.rx_control, &p).is_none());
        assert!(open_packet(&s.rx_data, &wrong.rx_control, &p).is_none());
    }
    #[test]
    fn malformed_body_and_data_dispatch() {
        for msg in [ping(), state()] {
            let b = msg.body();
            assert!(Control::parse(&b) == Some(msg));
            for len in 0..b.len() {
                assert!(Control::parse(&b[..len]).is_none());
            }
            for len in [0u16, 1, 3, 4, 31, 65, 67, u16::MAX] {
                let mut bad = b.clone();
                bad[2..4].copy_from_slice(&len.to_be_bytes());
                assert!(Control::parse(&bad).is_none());
            }
            assert_eq!(b[1], 0);
            for reserved in 1..=u8::MAX {
                let mut bad = b.clone();
                bad[1] = reserved;
                assert!(Control::parse(&bad).is_none());
            }
            let mut bad = b.clone();
            bad.push(1);
            assert!(Control::parse(&bad).is_none());
            let mut padded = b.clone();
            padded.resize(160, 0);
            assert!(Control::parse(&padded).is_some());
        }
        let k = PortKeys::base(&[4; 32], true);
        let mut ip = vec![0; 20];
        ip[0] = 0x45;
        ip[3] = 20;
        let p = Tx::new(k.tx_data).seal(&ip, 0).unwrap();
        assert!(open_packet(&k.tx_data, &k.tx_control, &p) == Some(Packet::Data(ip)));
    }
}
