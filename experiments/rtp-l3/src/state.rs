//! Sans-I/O session lifecycle. All times are monotonic whole seconds supplied by
//! the caller. Only authenticated, direction-checked control enters these APIs.
//! The runtime binds a port successfully before committing a rotation, creates
//! fresh secrets/Tx streams, sends returned actions, and closes retired sockets.
use crate::control::Control;
use rand::{rngs::OsRng, Rng};
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Path {
    pub port: u16,
    pub peer: SocketAddr,
}
#[derive(Clone)]
pub struct Send {
    pub path: Path,
    pub message: Control,
}
#[derive(Clone)]
pub struct Session {
    pub inner: Ipv4Addr,
    pub path: Path,
    pub seq: u32,
    pub last_seen: u64,
    pub acknowledged_epoch: u32,
    announce: Option<(u64, usize)>,
}
#[derive(Clone)]
struct Port {
    number: u16,
    secret: [u8; 32],
    close_after: u64,
}

pub fn rotation_delay(mean: u64) -> Option<u64> {
    if !(2..=u64::MAX / 3).contains(&mean) {
        return None;
    }
    Some(OsRng.gen_range(mean / 2..=mean * 3 / 2))
}
pub fn keepalive_delay(mean: u64) -> Option<u64> {
    if !(5..=u64::MAX / 6).contains(&mean) {
        return None;
    }
    Some(OsRng.gen_range(mean * 4 / 5..=mean * 6 / 5))
}

pub struct Server {
    pub instance: u64,
    pub epoch: u32,
    base: u16,
    current: Port,
    previous: Option<Port>,
    own: Ipv4Addr,
    network: u32,
    mask: u32,
    grace: u16,
    expiry: u64,
    limit: usize,
    sessions: BTreeMap<u64, Session>,
    pub conflicts: u64,
}
impl Server {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        instance: u64,
        own: Ipv4Addr,
        prefix: u8,
        base: u16,
        port: u16,
        secret: [u8; 32],
        grace: u16,
        expiry: u64,
        limit: usize,
    ) -> Option<Self> {
        if prefix > 32 || base == 0 || port == 0 || base == port || expiry == 0 || limit == 0 {
            return None;
        }
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        };
        Some(Self {
            instance,
            epoch: 0,
            base,
            current: Port {
                number: port,
                secret,
                close_after: 0,
            },
            previous: None,
            own,
            network: u32::from(own) & mask,
            mask,
            grace,
            expiry,
            limit,
            sessions: BTreeMap::new(),
            conflicts: 0,
        })
    }
    pub fn sessions(&self) -> &BTreeMap<u64, Session> {
        &self.sessions
    }
    pub fn ports(&self) -> Vec<u16> {
        let mut p = vec![self.base, self.current.number];
        if let Some(old) = &self.previous {
            p.push(old.number);
        }
        p
    }
    pub fn secret(&self, port: u16) -> Option<&[u8; 32]> {
        if port == self.current.number {
            Some(&self.current.secret)
        } else {
            self.previous
                .as_ref()
                .filter(|p| p.number == port)
                .map(|p| &p.secret)
        }
    }
    fn state(&self, id: u64, echo: u32) -> Control {
        Control::State {
            session: id,
            echo,
            instance: self.instance,
            epoch: self.epoch,
            port: self.current.number,
            secret: self.current.secret,
            base: self.base,
            grace: self.grace,
        }
    }
    /// Returns reply and displaced session id. Replays never refresh liveness.
    pub fn ping(&mut self, path: Path, msg: &Control, now: u64) -> Option<(Send, Option<u64>)> {
        self.expire(now);
        let Control::Ping {
            session,
            inner,
            seq,
            instance,
            epoch,
        } = *msg
        else {
            return None;
        };
        if !self.ports().contains(&path.port)
            || seq == 0
            || inner == self.own
            || u32::from(inner) & self.mask != self.network
        {
            return None;
        }
        if self.sessions.get(&session).is_some_and(|s| seq <= s.seq) {
            return None;
        }
        // One tuple on a socket has one owner; avoid ambiguous upstream routing.
        if self
            .sessions
            .iter()
            .any(|(id, s)| *id != session && s.path == path && s.inner != inner)
        {
            return None;
        }
        let displaced = self
            .sessions
            .iter()
            .find(|(id, s)| **id != session && s.inner == inner)
            .map(|(id, _)| *id);
        if !self.sessions.contains_key(&session)
            && displaced.is_none()
            && self.sessions.len() >= self.limit
        {
            return None;
        }
        if let Some(id) = displaced {
            self.sessions.remove(&id);
            self.conflicts += 1;
        }
        let acknowledged =
            instance == self.instance && epoch == self.epoch && path.port == self.current.number;
        self.sessions.insert(
            session,
            Session {
                inner,
                path,
                seq,
                last_seen: now,
                acknowledged_epoch: if acknowledged { self.epoch } else { 0 },
                announce: if acknowledged { None } else { Some((now, 0)) },
            },
        );
        Some((
            Send {
                path,
                message: self.state(session, seq),
            },
            displaced,
        ))
    }
    pub fn upstream(&mut self, path: Path, now: u64) -> bool {
        self.expire(now);
        if !self.ports().contains(&path.port) {
            return false;
        }
        if let Some(s) = self.sessions.values_mut().find(|s| s.path == path) {
            s.last_seen = now;
            true
        } else {
            false
        }
    }
    pub fn downstream(&mut self, inner: Ipv4Addr, now: u64) -> Option<Path> {
        self.expire(now);
        self.sessions
            .values()
            .find(|s| s.inner == inner)
            .map(|s| s.path)
    }
    fn expire(&mut self, now: u64) {
        let expiry = self.expiry;
        self.sessions
            .retain(|_, s| now.saturating_sub(s.last_seen) < expiry);
    }
    /// Rotate only after successful bind with a fresh secret. Oldest port is
    /// evicted if needed. Return its number so the runtime closes it immediately.
    pub fn rotate(
        &mut self,
        port: u16,
        secret: [u8; 32],
        now: u64,
    ) -> Option<(Vec<Send>, Option<u16>)> {
        if port == 0 || self.ports().contains(&port) || self.epoch == u32::MAX {
            return None;
        }
        self.expire(now);
        let closed = self.previous.take().map(|p| p.number);
        if let Some(p) = closed {
            self.sessions.retain(|_, s| s.path.port != p);
        }
        let old = std::mem::replace(
            &mut self.current,
            Port {
                number: port,
                secret,
                close_after: 0,
            },
        );
        self.previous = Some(Port {
            close_after: now.saturating_add(self.grace as u64),
            ..old
        });
        self.epoch += 1;
        for s in self.sessions.values_mut() {
            s.announce = Some((now, 0));
        }
        let sends = self
            .sessions
            .iter()
            .map(|(id, s)| Send {
                path: s.path,
                message: self.state(*id, 0),
            })
            .collect();
        Some((sends, closed))
    }
    /// Retry at +1,+2,+4 seconds from announcement; close after both grace
    /// elapsed and last session left. A following rotation bounds socket count.
    pub fn tick(&mut self, now: u64) -> (Vec<Send>, Vec<u16>) {
        self.expire(now);
        let mut ids = Vec::new();
        for (id, s) in &mut self.sessions {
            if let Some((start, index)) = s.announce {
                if index < 3 && now.saturating_sub(start) >= [1, 2, 4][index] {
                    ids.push((*id, s.path));
                    s.announce = if index == 2 {
                        None
                    } else {
                        Some((start, index + 1))
                    };
                }
            }
        }
        let sends = ids
            .into_iter()
            .map(|(id, path)| Send {
                path,
                message: self.state(id, 0),
            })
            .collect();
        let mut closed = Vec::new();
        if self.previous.as_ref().is_some_and(|p| {
            now >= p.close_after && !self.sessions.values().any(|s| s.path.port == p.number)
        }) {
            closed.push(self.previous.take().unwrap().number);
        }
        (sends, closed)
    }
}

#[derive(Clone)]
pub struct ClientPort {
    pub port: u16,
    pub secret: [u8; 32],
}
struct Pending {
    seq: u32,
    attempts: u8,
    deadline: u64,
}
pub struct Client {
    pub session: u64,
    pub inner: Ipv4Addr,
    pub instance: Option<u64>,
    pub epoch: u32,
    pub current: u16,
    pub previous: Option<(ClientPort, u64)>,
    pub rotation: Option<ClientPort>,
    base: u16,
    seq: u32,
    pending: Option<Pending>,
    last_tx: u64,
    last_rx: u64,
    interval: u64,
    backoff: u64,
    retry_at: Option<u64>,
    recovering: bool,
    pub fallbacks: u64,
    pub pings_unanswered: u64,
}
impl Client {
    pub fn new(session: u64, inner: Ipv4Addr, base: u16, interval: u64, now: u64) -> Option<Self> {
        if base == 0 || interval == 0 {
            return None;
        }
        Some(Self {
            session,
            inner,
            instance: None,
            epoch: 0,
            current: base,
            previous: None,
            rotation: None,
            base,
            seq: 0,
            pending: None,
            last_tx: now,
            last_rx: now,
            interval,
            backoff: 1,
            retry_at: None,
            recovering: false,
            fallbacks: 0,
            pings_unanswered: 0,
        })
    }
    /// None on sequence exhaustion: restart with a new random session id.
    pub fn ping(&mut self, now: u64) -> Option<Control> {
        self.seq = self.seq.checked_add(1)?;
        self.pending = Some(Pending {
            seq: self.seq,
            attempts: 1,
            deadline: now.saturating_add(2),
        });
        self.last_tx = now;
        Some(self.ping_message())
    }
    fn ping_message(&self) -> Control {
        Control::Ping {
            session: self.session,
            inner: self.inner,
            seq: self.seq,
            instance: self.instance.unwrap_or(0),
            epoch: self.epoch,
        }
    }
    /// The runtime may sample `keepalive_delay` for each new idle window;
    /// explicit intervals keep timer tests deterministic.
    pub fn set_keepalive_interval(&mut self, seconds: u64) -> bool {
        if seconds == 0 {
            return false;
        }
        self.interval = seconds;
        true
    }
    pub fn sent_data(&mut self, now: u64) {
        self.last_tx = now;
    }
    pub fn received_data(&mut self, port: u16, now: u64) -> bool {
        if !self.knows(port, now) {
            return false;
        }
        self.last_rx = now;
        true
    }
    pub fn knows(&self, port: u16, now: u64) -> bool {
        port == self.base
            || self.rotation.as_ref().is_some_and(|p| p.port == port)
            || self
                .previous
                .as_ref()
                .is_some_and(|(p, until)| p.port == port && now < *until)
    }
    /// Runtime first authenticates using the key of the source port and checks
    /// the configured server IP. An echo is consumed exactly once.
    pub fn state(&mut self, source: u16, msg: &Control, now: u64) -> Option<Control> {
        self.accept_state(source, msg, now).ok().flatten()
    }
    /// Distinguish accepted completion (Ok(None)) from an ignored reply.
    /// Reasons contain no secrets and are stable diagnostic labels.
    pub fn accept_state(
        &mut self,
        source: u16,
        msg: &Control,
        now: u64,
    ) -> Result<Option<Control>, &'static str> {
        let Control::State {
            session,
            echo,
            instance,
            epoch,
            port,
            secret,
            base,
            grace,
        } = *msg
        else {
            return Err("wrong_direction");
        };
        let fresh_echo = echo != 0 && self.pending.as_ref().is_some_and(|p| p.seq == echo);
        if session != self.session {
            return Err("wrong_session");
        }
        if base != self.base {
            return Err("wrong_base");
        }
        if port == 0 || port == base {
            return Err("invalid_rotation_port");
        }
        if !self.knows(source, now) {
            return Err("unknown_source_port");
        }
        let changed = self.instance != Some(instance);
        if changed && !fresh_echo {
            return Err("instance_requires_fresh_echo");
        }
        if !changed
            && (epoch < self.epoch
                || (epoch == self.epoch
                    && self
                        .rotation
                        .as_ref()
                        .is_some_and(|p| p.port != port || p.secret != secret)))
        {
            return Err("stale_epoch_or_conflicting_secret");
        }
        // Equal-epoch unsolicited/replayed STATE must be a no-op; it cannot
        // reset keepalive retries or extend the previous port's grace.
        if !changed && epoch == self.epoch && !fresh_echo {
            return Err(if echo == 0 {
                "equal_epoch_announcement"
            } else {
                "echo_not_pending"
            });
        }
        let switched = changed || epoch > self.epoch || self.current != port;
        if changed {
            self.previous = None;
            self.rotation = None;
        } else if switched {
            self.previous = self
                .rotation
                .take()
                .filter(|p| p.port != port)
                .map(|p| (p, now.saturating_add(grace as u64)));
        }
        self.instance = Some(instance);
        self.epoch = epoch;
        self.current = port;
        self.rotation = Some(ClientPort { port, secret });
        self.last_rx = now;
        self.pending = None;
        self.retry_at = None;
        self.recovering = false;
        self.backoff = 1;
        if switched {
            Ok(self.ping(now))
        } else {
            Ok(None)
        }
    }
    pub fn pending_sequence(&self) -> Option<u32> {
        self.pending.as_ref().map(|p| p.seq)
    }
    /// Call startup ping explicitly. Retries use fresh sequences, since the
    /// server deliberately ignores duplicate PINGs. Only the latest is pending.
    pub fn tick(&mut self, now: u64) -> Option<Control> {
        if self
            .previous
            .as_ref()
            .is_some_and(|(_, until)| now >= *until)
        {
            self.previous = None;
        }
        if self.recovering {
            if now < self.retry_at.unwrap() {
                return None;
            }
            self.pings_unanswered += 1;
            let msg = self.ping(now)?;
            self.retry_at = Some(now.saturating_add(self.backoff));
            self.backoff = (self.backoff * 2).min(30);
            return Some(msg);
        }
        if let Some(p) = &self.pending {
            if now < p.deadline {
                return None;
            }
            self.pings_unanswered += 1;
            if p.attempts < 3 {
                let attempts = p.attempts + 1;
                let msg = self.ping(now)?;
                self.pending.as_mut().unwrap().attempts = attempts;
                return Some(msg);
            }
            self.pending = None;
            self.current = self.base;
            self.recovering = true;
            self.fallbacks += 1;
            self.retry_at = Some(now.saturating_add(self.backoff));
            self.backoff = (self.backoff * 2).min(30);
            return None;
        }
        if now.saturating_sub(self.last_tx) >= self.interval
            || now.saturating_sub(self.last_rx) >= self.interval
        {
            self.ping(now)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn addr(n: u8) -> Ipv4Addr {
        Ipv4Addr::new(10, 0, 0, n)
    }
    fn path(port: u16, n: u16) -> Path {
        Path {
            port,
            peer: SocketAddr::from(([192, 0, 2, 1], n)),
        }
    }
    fn server() -> Server {
        Server::new(77, addr(1), 24, 4000, 4001, [1; 32], 60, 90, 8).unwrap()
    }
    fn client(id: u64, n: u8) -> Client {
        Client::new(id, addr(n), 4000, 15, 0).unwrap()
    }
    fn ping(id: u64, n: u8, seq: u32) -> Control {
        Control::Ping {
            session: id,
            inner: addr(n),
            seq,
            instance: 77,
            epoch: 0,
        }
    }
    fn state(echo: u32, instance: u64, epoch: u32, port: u16) -> Control {
        Control::State {
            session: 1,
            echo,
            instance,
            epoch,
            port,
            secret: [port as u8; 32],
            base: 4000,
            grace: 60,
        }
    }
    #[test]
    fn sessions_fresh_ping_only_roaming_conflicts_and_expiry() {
        let mut s = server();
        let a = path(4000, 100);
        let b = path(4001, 200);
        assert!(!s.upstream(a, 0));
        assert!(s.downstream(addr(2), 0).is_none());
        assert!(s.ping(a, &ping(1, 2, 1), 0).is_some());
        assert!(s.ping(b, &ping(1, 2, 1), 1).is_none());
        assert_eq!(s.downstream(addr(2), 1), Some(a));
        assert!(s.ping(b, &ping(1, 2, 2), 2).is_some());
        assert!(!s.upstream(a, 3));
        assert!(s.upstream(b, 10));
        assert_eq!(s.downstream(addr(2), 99), Some(b));
        assert!(s.downstream(addr(2), 100).is_none());
        assert!(s.ping(a, &ping(1, 2, 3), 101).is_some());
        assert_eq!(s.ping(b, &ping(2, 2, 1), 102).unwrap().1, Some(1));
        assert_eq!(s.conflicts, 1);
        assert_eq!(s.sessions().len(), 1);
        assert!(s.ping(a, &ping(3, 1, 1), 103).is_none());
        let outside = Control::Ping {
            session: 3,
            inner: Ipv4Addr::new(10, 0, 1, 2),
            seq: 1,
            instance: 0,
            epoch: 0,
        };
        assert!(s.ping(a, &outside, 103).is_none());
        assert!(s.ping(path(4999, 100), &ping(3, 3, 1), 103).is_none());
        assert!(s.ping(b, &ping(3, 3, 1), 103).is_none()); // ambiguous tuple
        assert!(s.ping(a, &ping(3, 3, 0), 103).is_none());
    }
    #[test]
    fn replay_does_not_extend_session_and_capacity_is_bounded() {
        let mut s = server();
        s.limit = 1;
        s.ping(path(4000, 1), &ping(1, 2, 1), 0).unwrap();
        assert!(s.ping(path(4000, 2), &ping(2, 3, 1), 1).is_none());
        assert!(s.ping(path(4000, 2), &ping(1, 2, 1), 89).is_none());
        assert!(s.downstream(addr(2), 90).is_none());
    }
    #[test]
    fn rotations_retries_ack_grace_and_three_socket_limit() {
        let mut s = server();
        let a = path(4001, 1);
        s.ping(a, &ping(1, 2, 1), 0).unwrap();
        assert!(s.rotate(4000, [9; 32], 0).is_none());
        let (ann, closed) = s.rotate(4002, [2; 32], 1).unwrap();
        assert_eq!(ann.len(), 1);
        assert_eq!(ann[0].path, a);
        assert_eq!(closed, None);
        assert_eq!(s.tick(1).0.len(), 0);
        for t in [2, 3, 5] {
            assert_eq!(s.tick(t).0.len(), 1);
        }
        assert_eq!(s.tick(6).0.len(), 0);
        let ack = Control::Ping {
            session: 1,
            inner: addr(2),
            seq: 2,
            instance: 77,
            epoch: 1,
        };
        assert!(s.ping(path(4002, 2), &ack, 6).is_some());
        assert_eq!(s.sessions()[&1].acknowledged_epoch, 1);
        assert!(s.tick(60).1.is_empty());
        assert_eq!(s.tick(61).1, vec![4001]);
        assert!(s.secret(4001).is_none());
        s.rotate(4003, [3; 32], 62).unwrap();
        // Last session still on 4002, so grace alone cannot close it.
        assert!(s.upstream(path(4002, 2), 80));
        assert!(s.upstream(path(4002, 2), 120));
        assert!(s.tick(122).1.is_empty());
        assert_eq!(s.rotate(4004, [4; 32], 123).unwrap().1, Some(4002));
        assert_eq!(s.ports().len(), 3);
        assert!(!s.upstream(path(4002, 2), 123));
        assert!(s.sessions().is_empty());
    }
    #[test]
    fn client_instance_epoch_replay_and_grace_rules() {
        let mut c = client(1, 2);
        c.ping(0).unwrap();
        assert!(c.state(4000, &state(0, 77, 0, 4001), 0).is_none());
        assert_eq!(c.current, 4000);
        assert!(c.state(4000, &state(1, 77, 0, 4001), 1).is_some());
        assert_eq!(c.current, 4001);
        assert!(c.state(4001, &state(0, 77, 1, 4002), 2).is_some());
        assert_eq!(c.current, 4002);
        assert!(c.state(4001, &state(0, 77, 0, 4001), 3).is_none());
        assert_eq!(c.current, 4002);
        assert!(c.state(4002, &state(0, 77, 1, 4003), 3).is_none());
        assert!(c.state(4999, &state(0, 77, 2, 4003), 3).is_none());
        assert!(c.state(4002, &state(0, 77, 1, 4002), 61).is_none());
        assert!(!c.knows(4001, 62)); // replay did not extend grace
        assert!(c.state(4000, &state(1, 66, 0, 4004), 63).is_none()); // consumed echo
        c.ping(64).unwrap();
        let seq = c.seq;
        assert!(c.state(4000, &state(seq, 88, 0, 4004), 65).is_some());
        assert_eq!(c.instance, Some(88));
        assert_eq!(c.current, 4004);
        assert!(c.previous.is_none());
        assert!(c.state(4000, &state(seq, 66, 0, 4003), 66).is_none());
    }
    #[test]
    fn keepalive_either_idle_direction_retries_fallback_backoff() {
        for idle_receive in [true, false] {
            let mut c = client(1, 2);
            for t in 0..15 {
                if idle_receive {
                    c.sent_data(t);
                } else {
                    c.received_data(4000, t);
                }
                assert!(c.tick(t).is_none());
            }
            assert!(c.tick(15).is_some());
            assert!(c.tick(16).is_none());
            assert!(c.tick(17).is_some());
            assert_eq!(c.seq, 2);
            assert!(c.tick(19).is_some());
            assert_eq!(c.seq, 3);
            assert!(c.tick(21).is_none());
            assert_eq!(c.fallbacks, 1);
            assert_eq!(c.current, 4000);
            assert!(c.tick(21).is_none());
            assert!(c.tick(22).is_some());
            for t in [24, 28, 36, 52, 82, 112] {
                assert!(c.tick(t - 1).is_none());
                assert!(c.tick(t).is_some());
            }
            assert_eq!(c.fallbacks, 1);
        }
        let mut c = client(1, 2);
        for t in 0..100 {
            c.sent_data(t);
            c.received_data(4000, t);
            assert!(c.tick(t).is_none());
        }
        c.seq = u32::MAX;
        assert!(c.ping(101).is_none());
    }
    #[test]
    fn missed_announcement_recovered_by_ping_and_restart() {
        let mut s = server();
        let mut c = client(1, 2);
        let p = c.ping(0).unwrap();
        let reply = s.ping(path(4000, 1), &p, 0).unwrap().0;
        let ack = c.state(4000, &reply.message, 0).unwrap();
        s.ping(path(c.current, 1), &ack, 0).unwrap();
        s.rotate(4002, [2; 32], 1).unwrap(); // deliberately lose all announcements
        let p = c.ping(15).unwrap();
        let reply = s.ping(path(c.current, 1), &p, 15).unwrap().0;
        let ack = c.state(c.current, &reply.message, 15).unwrap();
        assert_eq!(c.current, 4002);
        s.ping(path(4002, 2), &ack, 15).unwrap();
        assert_eq!(s.downstream(addr(2), 15), Some(path(4002, 2)));
        for t in [17, 19, 21, 22] {
            c.tick(t);
        }
        assert_eq!(c.current, 4000);
        assert_eq!(c.fallbacks, 1);
        let mut restarted = server();
        restarted.instance = 88;
        let p = c.ping(23).unwrap();
        let reply = restarted.ping(path(4000, 1), &p, 23).unwrap().0;
        assert!(c.state(4000, &reply.message, 23).is_some());
        assert_eq!(c.instance, Some(88));
    }
    #[test]
    fn two_clients_loss_retries_and_rotation_acknowledgements() {
        let mut s = server();
        let mut a = client(1, 2);
        let mut b = client(2, 3);
        for (c, tuple) in [(&mut a, 1), (&mut b, 2)] {
            let first = c.ping(0).unwrap();
            s.ping(path(4000, tuple), &first, 0).unwrap(); // reply lost
            let retry = c.tick(2).unwrap();
            let reply = s.ping(path(4000, tuple), &retry, 2).unwrap().0;
            let ack = c.state(4000, &reply.message, 2).unwrap();
            let reply = s.ping(path(c.current, tuple), &ack, 2).unwrap().0;
            assert!(c.state(c.current, &reply.message, 2).is_none());
        }
        assert_eq!(s.downstream(addr(2), 3), Some(path(4001, 1)));
        assert_eq!(s.downstream(addr(3), 3), Some(path(4001, 2)));
        let announcements = s.rotate(4002, [2; 32], 3).unwrap().0;
        for send in announcements {
            let c = if send.path.peer.port() == 1 {
                &mut a
            } else {
                &mut b
            };
            let ack = c.state(4001, &send.message, 3).unwrap();
            // A PING with the correct epoch on the old socket is not an ACK.
            s.ping(send.path, &ack, 3).unwrap();
        }
        assert_eq!(s.tick(4).0.len(), 2);
        for (c, tuple) in [(&mut a, 11), (&mut b, 22)] {
            let ack = c.ping(4).unwrap();
            let reply = s.ping(path(4002, tuple), &ack, 4).unwrap().0;
            c.state(4002, &reply.message, 4);
        }
        assert!(s.tick(5).0.is_empty());
        assert_eq!(s.downstream(addr(2), 5), Some(path(4002, 11)));
        assert_eq!(s.downstream(addr(3), 5), Some(path(4002, 22)));
        assert_eq!(s.tick(63).1, vec![4001]);
        s.epoch = u32::MAX;
        assert!(s.rotate(4003, [3; 32], 64).is_none());
    }

    #[test]
    fn reused_numeric_port_replaces_secret_without_ambiguous_grace() {
        let mut c = client(1, 2);
        c.ping(0).unwrap();
        c.state(4000, &state(1, 77, 0, 4001), 0).unwrap();
        let mut announcement = state(0, 77, 3, 4001);
        if let Control::State { secret, .. } = &mut announcement {
            *secret = [99; 32];
        }
        assert!(c.state(4000, &announcement, 10).is_some());
        assert_eq!(c.rotation.as_ref().unwrap().secret, [99; 32]);
        assert!(c.previous.is_none());
    }

    #[test]
    fn diagnostic_outcomes_distinguish_completion_and_superseded_echo() {
        let mut c = client(1, 2);
        c.ping(0).unwrap();
        assert!(c
            .accept_state(4000, &state(1, 77, 0, 4001), 0)
            .unwrap()
            .is_some());
        // Bootstrap generated seq 2 on the new path; its completion sends nothing.
        assert_eq!(c.pending_sequence(), Some(2));
        assert!(matches!(
            c.accept_state(4001, &state(2, 77, 0, 4001), 1),
            Ok(None)
        ));
        assert_eq!(c.pending_sequence(), None);
        c.ping(15).unwrap();
        c.tick(17).unwrap(); // seq 4 supersedes seq 3
        assert_eq!(c.pending_sequence(), Some(4));
        assert!(matches!(
            c.accept_state(4001, &state(3, 77, 0, 4001), 17),
            Err("echo_not_pending")
        ));
        assert_eq!(c.pending_sequence(), Some(4));
        assert!(matches!(
            c.accept_state(4001, &state(4, 77, 0, 4001), 18),
            Ok(None)
        ));
        assert_eq!(c.pending_sequence(), None);
        assert_eq!(c.fallbacks, 0);
    }

    #[test]
    fn jitter_bounds_and_configuration() {
        for _ in 0..1000 {
            assert!((150..=450).contains(&rotation_delay(300).unwrap()));
            assert!((12..=18).contains(&keepalive_delay(15).unwrap()));
        }
        assert!(rotation_delay(u64::MAX).is_none());
        assert!(keepalive_delay(0).is_none());
        assert!(Client::new(1, addr(2), 0, 15, 0).is_none());
        assert!(Server::new(1, addr(1), 33, 4000, 4001, [1; 32], 60, 90, 8).is_none());
    }
}
