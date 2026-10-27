//! Deterministic path/activation, health timers and bounded downstream queues.
//! Times are monotonic milliseconds supplied by the owner; no host I/O here.
use super::codec::Control;
use std::{
    collections::{BTreeMap, VecDeque},
    net::{Ipv4Addr, SocketAddr},
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Validated {
    pub peer: SocketAddr,
    pub attempt: u64,
    pub token: [u8; 16],
}
struct Candidate {
    path: Validated,
    challenge: [u8; 16],
    expires: u64,
}
#[derive(Default)]
pub struct Path {
    highest: Option<u64>,
    candidate: Option<Candidate>,
    pub ready: Option<Validated>,
    pub committed: Option<Validated>,
    completed_challenge: Option<[u8; 16]>,
}
impl Path {
    pub fn open(
        &mut self,
        peer: SocketAddr,
        attempt: u64,
        token: [u8; 16],
        challenge: [u8; 16],
        now: u64,
    ) -> Option<Control> {
        let path = Validated {
            peer,
            attempt,
            token,
        };
        if self.ready == Some(path) {
            return Some(Control::PathReady { attempt, token });
        }
        if let Some(c) = &self.candidate {
            if c.path == path && now < c.expires {
                return Some(Control::PathChallenge {
                    attempt,
                    token,
                    challenge: c.challenge,
                });
            }
        }
        if self.highest.is_some_and(|h| attempt <= h) {
            return None;
        }
        self.highest = Some(attempt);
        self.candidate = Some(Candidate {
            path,
            challenge,
            expires: now.saturating_add(5000),
        });
        Some(Control::PathChallenge {
            attempt,
            token,
            challenge,
        })
    }
    pub fn response(
        &mut self,
        peer: SocketAddr,
        attempt: u64,
        token: [u8; 16],
        challenge: [u8; 16],
        now: u64,
    ) -> Option<Control> {
        let path = Validated {
            peer,
            attempt,
            token,
        };
        // Only completed attempt duplicates may resend READY. An arbitrary
        // challenge cannot complete a second path or change its tuple.
        if let Some(c) = &self.candidate {
            if c.path == path && c.challenge == challenge && now < c.expires {
                self.ready = Some(path);
                self.completed_challenge = Some(challenge);
                self.candidate = None;
                return Some(Control::PathReady { attempt, token });
            }
        }
        if self.ready == Some(path) && self.completed_challenge == Some(challenge) {
            return Some(Control::PathReady { attempt, token });
        }
        None
    }
    pub fn data_from(&self, peer: SocketAddr) -> bool {
        self.committed.is_some_and(|p| p.peer == peer)
    }
    pub fn can_activate(&self, peer: SocketAddr, attempt: u64, token: [u8; 16]) -> bool {
        self.ready
            == Some(Validated {
                peer,
                attempt,
                token,
            })
    }
}
#[derive(Clone, Copy)]
pub struct Owner {
    pub allocation: Uuid,
    pub activation: u64,
}
#[derive(Default)]
pub struct Routes {
    owners: BTreeMap<Uuid, Owner>,
    superseded: BTreeMap<Uuid, u64>,
}
impl Routes {
    /// Call only after path validation. Returns old owner on a committed change.
    pub fn activate(
        &mut self,
        client: Uuid,
        allocation: Uuid,
        activation: u64,
        now: u64,
    ) -> Result<Option<Uuid>, &'static str> {
        if self.superseded.contains_key(&allocation) {
            return Err("superseded_allocation");
        }
        if let Some(old) = self.owners.get(&client) {
            if activation < old.activation
                || (activation == old.activation && allocation != old.allocation)
            {
                return Err("stale_activation");
            }
            if activation == old.activation {
                return Ok(None);
            }
            let previous = old.allocation;
            self.owners.insert(
                client,
                Owner {
                    allocation,
                    activation,
                },
            );
            if previous != allocation {
                self.superseded.insert(previous, now.saturating_add(10_000));
                return Ok(Some(previous));
            }
            return Ok(None);
        }
        self.owners.insert(
            client,
            Owner {
                allocation,
                activation,
            },
        );
        Ok(None)
    }
    pub fn owner(&self, client: Uuid) -> Option<Uuid> {
        self.owners.get(&client).map(|o| o.allocation)
    }
    pub fn upstream(&self, client: Uuid, allocation: Uuid, now: u64) -> bool {
        self.owner(client) == Some(allocation)
            || self
                .superseded
                .get(&allocation)
                .is_some_and(|until| now < *until)
    }
    pub fn retire(&mut self, allocation: Uuid) {
        // Keep high watermarks until all a client's leases are gone; retiring
        // the active route must not let a draining lease reclaim it.
        self.superseded.remove(&allocation);
    }
    pub fn remove_client(&mut self, client: Uuid, allocations: &[Uuid]) {
        self.owners.remove(&client);
        for id in allocations {
            self.superseded.remove(id);
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthAction {
    Ping([u8; 16]),
    Suspect,
}
struct Probe {
    token: [u8; 16],
    sent: u64,
    index: u8,
}
pub struct Health {
    last_in: u64,
    idle_at: u64,
    probe: Option<Probe>,
    suspect: bool,
}
impl Health {
    pub fn new(now: u64, idle_delay: u64) -> Self {
        Self {
            last_in: now,
            idle_at: now.saturating_add(idle_delay),
            probe: None,
            suspect: false,
        }
    }
    pub fn sent(&mut self, now: u64, idle_delay: u64) {
        self.idle_at = now.saturating_add(idle_delay);
    }
    pub fn received(&mut self, now: u64, pong: Option<[u8; 16]>) {
        self.last_in = now;
        self.suspect = false;
        if self.probe.as_ref().is_some_and(|p| Some(p.token) == pong) {
            self.probe = None;
        }
    }
    pub fn poll(&mut self, now: u64, token: [u8; 16]) -> Option<HealthAction> {
        if let Some(p) = &mut self.probe {
            let wait = match p.index {
                0 => 1000,
                1 => 2000,
                _ => 4000,
            };
            if now < p.sent.saturating_add(wait) {
                return None;
            }
            if p.index == 2 {
                self.probe = None;
                self.suspect = true;
                return Some(HealthAction::Suspect);
            }
            p.index += 1;
            p.token = token;
            p.sent = now;
            return Some(HealthAction::Ping(token));
        }
        if !self.suspect && (now >= self.idle_at || now.saturating_sub(self.last_in) >= 15_000) {
            self.probe = Some(Probe {
                token,
                sent: now,
                index: 0,
            });
            return Some(HealthAction::Ping(token));
        }
        None
    }
}
struct Buffered {
    at: u64,
    serial: u64,
    body: Vec<u8>,
}
#[derive(Default)]
pub struct Queues {
    clients: BTreeMap<Ipv4Addr, VecDeque<Buffered>>,
    bytes: usize,
    serial: u64,
    pub expired: u64,
    pub pressure: u64,
    pub unknown: u64,
}
impl Queues {
    pub fn register(&mut self, ip: Ipv4Addr) {
        self.clients.entry(ip).or_default();
    }
    pub fn remove(&mut self, ip: Ipv4Addr) {
        if let Some(q) = self.clients.remove(&ip) {
            self.bytes -= q.iter().map(|b| b.body.len()).sum::<usize>();
        }
    }
    fn drop_first(&mut self, ip: Ipv4Addr) {
        if let Some(b) = self.clients.get_mut(&ip).and_then(VecDeque::pop_front) {
            self.bytes -= b.body.len();
        }
    }
    pub fn expire(&mut self, now: u64) {
        for q in self.clients.values_mut() {
            while q.front().is_some_and(|b| now.saturating_sub(b.at) >= 1000) {
                let b = q.pop_front().unwrap();
                self.bytes -= b.body.len();
                self.expired += 1;
            }
        }
    }
    pub fn push(&mut self, ip: Ipv4Addr, body: Vec<u8>, now: u64) {
        self.expire(now);
        let Some(q) = self.clients.get_mut(&ip) else {
            self.unknown += 1;
            return;
        };
        if body.len() > crate::MTU {
            self.pressure += 1;
            return;
        }
        self.bytes += body.len();
        self.serial = self.serial.wrapping_add(1);
        q.push_back(Buffered {
            at: now,
            serial: self.serial,
            body,
        });
        while self.clients[&ip].len() > 256
            || self.clients[&ip]
                .iter()
                .map(|b| b.body.len())
                .sum::<usize>()
                > 256 * 1024
        {
            self.drop_first(ip);
            self.pressure += 1;
        }
        while self.bytes > 16 * 1024 * 1024 {
            let oldest = self
                .clients
                .iter()
                .filter_map(|(ip, q)| q.front().map(|b| (*ip, b.serial)))
                .min_by_key(|(_, s)| *s)
                .unwrap()
                .0;
            self.drop_first(oldest);
            self.pressure += 1;
        }
    }
    pub fn front(&mut self, ip: Ipv4Addr, now: u64) -> Option<&[u8]> {
        self.expire(now);
        self.clients.get(&ip)?.front().map(|b| b.body.as_slice())
    }
    pub fn pop(&mut self, ip: Ipv4Addr) {
        self.drop_first(ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tuple_challenges_cannot_roll_back_active_path() {
        let mut p = Path::default();
        let a = "127.0.0.1:1".parse().unwrap();
        let b = "127.0.0.1:2".parse().unwrap();
        assert!(p.open(a, 1, [1; 16], [2; 16], 0).is_some());
        assert!(!p.data_from(a));
        assert!(p.response(b, 1, [1; 16], [2; 16], 1).is_none());
        assert!(p.response(a, 1, [1; 16], [3; 16], 1).is_none());
        assert!(p.response(a, 1, [1; 16], [2; 16], 1).is_some());
        p.committed = p.ready;
        assert!(p.data_from(a));
        p.open(b, 2, [3; 16], [4; 16], 2);
        assert!(p.data_from(a));
        assert!(!p.data_from(b));
        assert!(p.open(a, 1, [1; 16], [2; 16], 3).is_some());
        assert!(p.response(b, 2, [3; 16], [4; 16], 3).is_some());
        assert!(p.open(a, 1, [1; 16], [2; 16], 4).is_none());
        assert!(p.response(a, 1, [1; 16], [2; 16], 4).is_none());
        assert!(p.data_from(a));
        p.committed = p.ready;
        assert!(p.data_from(b));
        assert!(!p.data_from(a));
    }
    #[test]
    fn activation_ack_retry_and_permanent_supersession() {
        let mut r = Routes::default();
        let c = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert_eq!(r.activate(c, a, 1, 0), Ok(None));
        assert_eq!(r.activate(c, b, 2, 1), Ok(Some(a)));
        assert_eq!(r.activate(c, b, 2, 2), Ok(None));
        assert!(r.activate(c, a, 3, 3).is_err());
        assert!(r.activate(c, a, 1, 3).is_err());
        assert!(r.upstream(c, a, 10000));
        assert!(!r.upstream(c, a, 10001));
        r.retire(b);
        assert!(r.activate(c, a, 4, 4).is_err());
    }
    #[test]
    fn idle_and_active_upload_probes_have_nonextendable_deadlines() {
        let mut h = Health::new(0, 15000);
        for n in (0..15000).step_by(1000) {
            h.sent(n, 15000);
            assert_eq!(h.poll(n, [0; 16]), None);
        }
        assert_eq!(h.poll(15000, [1; 16]), Some(HealthAction::Ping([1; 16])));
        h.received(15500, None);
        assert_eq!(h.poll(16000, [2; 16]), Some(HealthAction::Ping([2; 16])));
        h.received(16500, Some([1; 16]));
        assert_eq!(h.poll(18000, [3; 16]), Some(HealthAction::Ping([3; 16])));
        h.received(19000, None);
        assert_eq!(h.poll(22000, [4; 16]), Some(HealthAction::Suspect));
    }
    #[test]
    fn incoming_only_does_not_suppress_nat_ping_and_matching_pong_completes() {
        let mut h = Health::new(0, 15000);
        h.received(14999, None);
        assert_eq!(h.poll(15000, [1; 16]), Some(HealthAction::Ping([1; 16])));
        h.sent(15000, 15000);
        h.received(15001, Some([1; 16]));
        assert_eq!(h.poll(16000, [2; 16]), None);
    }
    #[test]
    fn queues_expire_drop_oldest_and_keep_fifo() {
        let mut q = Queues::default();
        let ip = Ipv4Addr::LOCALHOST;
        q.push(ip, vec![1], 0);
        assert_eq!(q.unknown, 1);
        q.register(ip);
        for i in 0..300 {
            q.push(ip, vec![i as u8; 1200], 0);
        }
        assert_eq!(q.clients[&ip].len(), 218);
        assert_eq!(q.pressure, 82);
        assert_eq!(q.front(ip, 999).unwrap()[0], 82);
        q.pop(ip);
        assert_eq!(q.front(ip, 999).unwrap()[0], 83);
        assert!(q.front(ip, 1000).is_none());
        assert_eq!(q.bytes, 0);
    }
}
