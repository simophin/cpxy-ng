//! RTP-only same-numeric-port coordination over the generic relay lease API.
use crate::ports::{Admission, Lease};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    net::{IpAddr, SocketAddr},
    sync::Mutex,
};
use udp_relay::management::{self, ClientConfig, Request, Spec};
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    pub management: ClientConfig,
    pub listen_ip: IpAddr,
    pub endpoint_ip: IpAddr,
}
pub struct Coordinated {
    pub local: Box<dyn Admission>,
    pub config: RelayConfig,
    pub boots: Mutex<HashMap<String, String>>,
}
impl Coordinated {
    fn spec(&self, l: &Lease) -> Spec {
        Spec {
            owner: self.config.management.owner.clone(),
            generation: l.generation.clone(),
            listen: SocketAddr::new(self.config.listen_ip, l.address.port()),
            upstream: SocketAddr::new(self.config.endpoint_ip, l.address.port()),
        }
    }
    fn prepare(&self, l: &Lease) -> io::Result<()> {
        let spec = self.spec(l);
        let r = management::call(
            &self.config.management,
            &Request::Prepare { spec: spec.clone() },
        )?;
        if !r.ready || !r.entries.iter().any(|e| e.spec == spec) {
            return Err(io::Error::other("relay READY mismatch"));
        }
        self.boots
            .lock()
            .unwrap()
            .insert(l.generation.clone(), r.boot);
        Ok(())
    }
    fn renew(&self, l: &Lease, renewal: bool) -> io::Result<()> {
        let spec = self.spec(l);
        let boot = self
            .boots
            .lock()
            .unwrap()
            .get(&l.generation)
            .cloned()
            .ok_or_else(|| io::Error::other("missing generation-bound READY"))?;
        let request = if renewal {
            Request::Renew {
                spec: spec.clone(),
                boot: boot.clone(),
            }
        } else {
            Request::Commit {
                spec: spec.clone(),
                boot: boot.clone(),
            }
        };
        let r = management::call(&self.config.management, &request)?;
        if !r.ready || r.boot != boot || !r.entries.iter().any(|e| e.spec == spec && e.committed) {
            return Err(io::Error::other("relay commit mismatch"));
        }
        Ok(())
    }
}
impl Admission for Coordinated {
    fn prepare_port(&self, l: &Lease) -> io::Result<()> {
        self.local.prepare_port(l)?;
        self.prepare(l)
    }
    fn commit_port(&self, l: &Lease) -> io::Result<()> {
        self.renew(l, false)?;
        self.local.commit_port(l)?;
        eprintln!(
            "{}",
            serde_json::json!({"event":"relay_committed","port":l.address.port(),"generation":l.generation,"boot":self.boots.lock().unwrap().get(&l.generation)})
        );
        Ok(())
    }
    fn retire_port(&self, l: &Lease) -> io::Result<()> {
        management::call(
            &self.config.management,
            &Request::Retire { spec: self.spec(l) },
        )?;
        self.local.retire_port(l)?;
        self.boots.lock().unwrap().remove(&l.generation);
        Ok(())
    }
    fn reconcile_ports(&self, leases: &[Lease]) -> io::Result<()> {
        self.local.reconcile_ports(leases)?;
        let inventory = management::call(&self.config.management, &Request::Inventory)?;
        if !inventory.ready {
            return Err(io::Error::other("relay admission degraded"));
        }
        for l in leases {
            if l.state == crate::ports::LeaseState::Retiring {
                continue;
            }
            self.prepare(l)?;
            if l.state == crate::ports::LeaseState::Active {
                self.renew(
                    l,
                    inventory
                        .entries
                        .iter()
                        .any(|e| e.spec == self.spec(l) && e.committed),
                )?;
            }
        }
        Ok(())
    }
}
