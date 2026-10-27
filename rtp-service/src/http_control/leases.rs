//! Serialized application owner. Firewall calls never hold the packet table lock.
use super::operations::{error_code, Operation, Operations};
use crate::addresses::Addresses;
use rand::seq::SliceRandom;
use rtp_l3::{
    v2::{
        codec::{Direction, Rx, Tx},
        http::{hex, random, Rpc},
        state::{Path, Queues, Routes},
    },
    worker::SocketProvider,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

pub struct Allocation {
    pub client: Uuid,
    pub inner: Ipv4Addr,
    pub socket: UdpSocket,
    pub tx: Tx,
    pub rx: Rx,
    pub path: Path,
    pub created: u64,
    pub last_seen: Option<u64>,
    pub retired: bool,
    pub activation: Option<u64>,
}
#[derive(Default)]
pub struct Table {
    pub allocations: BTreeMap<Uuid, Allocation>,
    pub routes: Routes,
    pub queues: Queues,
}
pub struct State {
    addresses: Addresses,
    pub table: Arc<Mutex<Table>>,
    provider: Arc<dyn SocketProvider>,
    bind: SocketAddr,
    low: u16,
    high: u16,
    prefix: u8,
    server: Ipv4Addr,
    retiring: BTreeMap<Uuid, Allocation>,
    quarantine: Vec<UdpSocket>,
    pub terminal: Vec<Uuid>,
}
impl State {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        addresses: Addresses,
        table: Arc<Mutex<Table>>,
        provider: Arc<dyn SocketProvider>,
        bind: SocketAddr,
        low: u16,
        high: u16,
        prefix: u8,
        server: Ipv4Addr,
    ) -> Self {
        Self {
            addresses,
            table,
            provider,
            bind,
            low,
            high,
            prefix,
            server,
            retiring: BTreeMap::new(),
            quarantine: Vec::new(),
            terminal: Vec::new(),
        }
    }
    fn apply(
        &mut self,
        op: &Operation,
        generation: &str,
        now: u64,
    ) -> (Result<Value, &'static str>, Option<Uuid>) {
        match op {
            Operation::Allocate(p) => (
                self.addresses
                    .allocate(p.client_id)
                    .map(|inner| json!({"inner":inner,"prefix":self.prefix,"server":self.server})),
                None,
            ),
            Operation::Create(p) => {
                if let Err(e) = self.addresses.claim(p.client_id, p.inner) {
                    return (Err(e), None);
                }
                let live = {
                    let table = self.table.lock().unwrap();
                    (
                        table.allocations.len() + self.retiring.len() + self.quarantine.len(),
                        table
                            .allocations
                            .values()
                            .chain(self.retiring.values())
                            .filter(|a| a.client == p.client_id)
                            .count(),
                    )
                };
                if live.0 >= 1024 || live.1 >= 2 {
                    return (Err("allocation_quota"), None);
                }
                let used: Vec<_> = {
                    let table = self.table.lock().unwrap();
                    table
                        .allocations
                        .values()
                        .chain(self.retiring.values())
                        .map(|a| a.socket.local_addr().unwrap().port())
                        .chain(
                            self.quarantine
                                .iter()
                                .filter_map(|s| s.local_addr().ok().map(|a| a.port())),
                        )
                        .collect()
                };
                // No configured range means bind(:0). Bound live/retiring
                // sockets remain reserved, so the kernel cannot reuse them.
                let mut ports: Vec<_> = if self.low == 0 {
                    vec![0]
                } else {
                    (self.low..=self.high)
                        .filter(|p| !used.contains(p))
                        .collect()
                };
                ports.shuffle(&mut rand::rngs::OsRng);
                let mut prepared = None;
                for port in ports {
                    let mut address = self.bind;
                    address.set_port(port);
                    match self.provider.prepare_port(address) {
                        Ok(socket) => {
                            prepared = Some(socket);
                            break;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
                        Err(_) => return (Err("firewall_unavailable"), None),
                    }
                }
                let Some(socket) = prepared else {
                    return (Err("port_pool_exhausted"), None);
                };
                let port = socket.local_addr().unwrap().port();
                if socket.set_nonblocking(true).is_err() || self.provider.commit_port(port).is_err()
                {
                    if self.provider.retire_port(port).is_err() {
                        self.quarantine.push(socket);
                    }
                    return (Err("firewall_unavailable"), None);
                }
                let id = Uuid::new_v4();
                let secret = random::<32>();
                let allocation = Allocation {
                    client: p.client_id,
                    inner: p.inner,
                    socket,
                    tx: Tx::new(&secret, *id.as_bytes(), Direction::Downstream, p.inner),
                    rx: Rx::new(&secret, *id.as_bytes(), Direction::Upstream, p.inner),
                    path: Path::default(),
                    created: now,
                    last_seen: None,
                    retired: false,
                    activation: None,
                };
                let mut table = self.table.lock().unwrap();
                let activation_floor = table
                    .routes
                    .next_activation(p.client_id)
                    .unwrap_or(u64::MAX);
                table.queues.register(p.inner);
                table.allocations.insert(id, allocation);
                (
                    Ok(
                        json!({"allocation_id":id,"generation":generation,"activation_floor":activation_floor,"port":port,"secret":hex(&secret),"mtu":1200,"startup_ms":30000,"inactivity_ms":120000,"rotation_ms":300000,"lead_ms":10000,"drain_ms":10000}),
                    ),
                    Some(id),
                )
            }
            Operation::Release(p) => {
                let mut table = self.table.lock().unwrap();
                if table
                    .allocations
                    .get(&p.allocation_id)
                    .is_some_and(|a| a.client != p.client_id)
                    || self
                        .retiring
                        .get(&p.allocation_id)
                        .is_some_and(|a| a.client != p.client_id)
                {
                    return (Err("address_conflict"), None);
                }
                if let Some(allocation) = table.allocations.remove(&p.allocation_id) {
                    table.routes.retire(p.allocation_id);
                    if !table
                        .allocations
                        .values()
                        .any(|a| a.client == allocation.client)
                    {
                        table.queues.remove(allocation.inner);
                        table.routes.remove_client(allocation.client, &[]);
                    }
                    self.retiring.insert(p.allocation_id, allocation);
                    self.terminal.push(p.allocation_id);
                }
                drop(table);
                self.reconcile_retired();
                (Ok(json!({"released":true})), None)
            }
        }
    }
    pub fn tick(&mut self, now: u64) {
        {
            let mut table = self.table.lock().unwrap();
            let expired: Vec<_> = table
                .allocations
                .iter()
                .filter(|(id, a)| {
                    a.retired
                        || table.routes.drained(**id, now)
                        || match a.last_seen {
                            None => now.saturating_sub(a.created) >= 30000,
                            Some(t) => now.saturating_sub(t) >= 120000,
                        }
                })
                .map(|(id, _)| *id)
                .collect();
            for id in expired {
                let allocation = table.allocations.remove(&id).unwrap();
                table.routes.retire(id);
                if !table
                    .allocations
                    .values()
                    .any(|a| a.client == allocation.client)
                {
                    table.queues.remove(allocation.inner);
                    table.routes.remove_client(allocation.client, &[]);
                }
                self.retiring.insert(id, allocation);
                self.terminal.push(id);
            }
        }
        self.reconcile_retired();
    }
    fn reconcile_retired(&mut self) {
        self.retiring.retain(|_, a| {
            self.provider
                .retire_port(a.socket.local_addr().unwrap().port())
                .is_err()
        });
        self.quarantine.retain(|s| {
            self.provider
                .retire_port(s.local_addr().unwrap().port())
                .is_err()
        });
    }
    pub fn shutdown(&mut self) -> std::io::Result<()> {
        {
            let mut table = self.table.lock().unwrap();
            let removed = std::mem::take(&mut table.allocations);
            self.retiring.extend(removed);
        }
        self.reconcile_retired();
        if self.retiring.is_empty() && self.quarantine.is_empty() {
            Ok(())
        } else {
            Err(std::io::Error::other(
                "UDP admission retirement remains uncertain; journal retained",
            ))
        }
    }
}
pub struct Controller {
    pub state: State,
    operations: Operations,
    generation: String,
}
impl Controller {
    pub fn new(state: State, generation: String) -> Self {
        Self {
            state,
            operations: Operations::new(8192),
            generation,
        }
    }
    pub fn rpc(&mut self, rpc: &Rpc, now: u64) -> Value {
        let operation = match Operation::parse(rpc) {
            Ok(op) => op,
            Err(e) => return rpc.error(error_code(e), e),
        };
        let state = &mut self.state;
        let generation = &self.generation;
        let result = self.operations.execute(&operation, generation, now, || {
            state.apply(&operation, generation, now)
        });
        for id in self.state.terminal.drain(..) {
            self.operations.terminal(id);
        }
        match result {
            Ok(v) => rpc.success(v),
            Err(e) => rpc.error(error_code(e), e),
        }
    }
    pub fn tick(&mut self, now: u64) {
        self.state.tick(now);
        for id in self.state.terminal.drain(..) {
            self.operations.terminal(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::operations::{Create, Release};
    use super::*;
    use std::{
        io,
        sync::atomic::{AtomicBool, Ordering},
    };
    struct Host {
        commit_fails: AtomicBool,
        retire_fails: AtomicBool,
    }
    impl SocketProvider for Host {
        fn prepare_port(&self, a: SocketAddr) -> io::Result<UdpSocket> {
            UdpSocket::bind(a)
        }
        fn commit_port(&self, _: u16) -> io::Result<()> {
            if self.commit_fails.load(Ordering::Relaxed) {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        }
        fn retire_port(&self, _: u16) -> io::Result<()> {
            if self.retire_fails.load(Ordering::Relaxed) {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        }
        fn reconcile_ports(&self) -> io::Result<()> {
            Ok(())
        }
        fn client_socket(&self, _: SocketAddr) -> io::Result<UdpSocket> {
            unreachable!()
        }
    }
    #[test]
    fn failed_commit_holds_socket_until_retirement_and_stale_release_cannot_retire_reuse() {
        let root = std::env::temp_dir().join(format!("rtp-lease-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let own = "10.0.0.1".parse().unwrap();
        let inner = "10.0.0.2".parse().unwrap();
        let addresses = Addresses::open(
            root.join("addresses"),
            own,
            24,
            inner,
            "10.0.0.254".parse().unwrap(),
            vec![],
        )
        .unwrap();
        let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
        let bind = probe.local_addr().unwrap();
        drop(probe);
        let host = Arc::new(Host {
            commit_fails: AtomicBool::new(true),
            retire_fails: AtomicBool::new(true),
        });
        let table = Arc::new(Mutex::new(Table::default()));
        let mut state = State::new(
            addresses,
            table.clone(),
            host.clone(),
            bind,
            bind.port(),
            bind.port(),
            24,
            own,
        );
        let client = Uuid::new_v4();
        let create = Operation::Create(Create {
            client_id: client,
            operation_id: Uuid::new_v4(),
            generation: "boot".into(),
            issued_ms: 0,
            inner,
        });
        assert_eq!(
            state.apply(&create, "boot", 0).0,
            Err("firewall_unavailable")
        );
        assert!(table.lock().unwrap().allocations.is_empty());
        assert!(UdpSocket::bind(bind).is_err());
        assert_eq!(
            state.apply(&create, "boot", 1).0,
            Err("port_pool_exhausted")
        );
        host.retire_fails.store(false, Ordering::Relaxed);
        host.commit_fails.store(false, Ordering::Relaxed);
        state.tick(1);
        let first = state.apply(&create, "boot", 2).1.unwrap();
        let release = Operation::Release(Release {
            client_id: client,
            operation_id: Uuid::new_v4(),
            generation: "boot".into(),
            issued_ms: 2,
            allocation_id: first,
        });
        assert!(state.apply(&release, "boot", 3).0.is_ok());
        let second = state.apply(&create, "boot", 4).1.unwrap();
        assert_ne!(first, second);
        assert!(state.apply(&release, "boot", 5).0.is_ok());
        assert!(table.lock().unwrap().allocations.contains_key(&second));
        state.tick(30003);
        assert!(table.lock().unwrap().allocations.contains_key(&second));
        state.tick(30004);
        assert!(table.lock().unwrap().allocations.is_empty());
        state.shutdown().unwrap();
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
        assert!(UdpSocket::bind(bind).is_ok());
    }
}
