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
                let mut ports: Vec<_> = (self.low..=self.high)
                    .filter(|p| !used.contains(p))
                    .collect();
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
                table.queues.register(p.inner);
                table.allocations.insert(id, allocation);
                (
                    Ok(
                        json!({"allocation_id":id,"generation":generation,"port":port,"secret":hex(&secret),"mtu":1200,"startup_ms":30000,"inactivity_ms":120000,"rotation_ms":300000,"lead_ms":10000,"drain_ms":10000}),
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
                .filter(|(_, a)| {
                    a.retired
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
