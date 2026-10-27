//! Idempotency is independent of HTTP envelope replay. One serialized owner.
use rtp_l3::v2::http::{Rpc, RETRY_MS};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, net::Ipv4Addr};
use uuid::Uuid;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Common {
    pub client_id: Uuid,
    pub operation_id: Uuid,
    pub generation: String,
    pub issued_ms: u64,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub client_id: Uuid,
    pub operation_id: Uuid,
    pub generation: String,
    pub issued_ms: u64,
    pub inner: Ipv4Addr,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub client_id: Uuid,
    pub operation_id: Uuid,
    pub generation: String,
    pub issued_ms: u64,
    pub allocation_id: Uuid,
}
#[derive(Clone)]
pub enum Operation {
    Allocate(Common),
    Create(Create),
    Release(Release),
}
impl Operation {
    pub fn parse(rpc: &Rpc) -> Result<Self, &'static str> {
        let params = Value::Object(rpc.params.clone());
        let op = match rpc.method.as_str() {
            "allocate_ip" => {
                Self::Allocate(serde_json::from_value(params).map_err(|_| "invalid_params")?)
            }
            "create_udp" => {
                Self::Create(serde_json::from_value(params).map_err(|_| "invalid_params")?)
            }
            "release_udp" => {
                Self::Release(serde_json::from_value(params).map_err(|_| "invalid_params")?)
            }
            _ => return Err("unknown_method"),
        };
        if op.client().is_nil() || op.id().is_nil() {
            return Err("invalid_params");
        }
        Ok(op)
    }
    pub fn client(&self) -> Uuid {
        match self {
            Self::Allocate(p) => p.client_id,
            Self::Create(p) => p.client_id,
            Self::Release(p) => p.client_id,
        }
    }
    pub fn id(&self) -> Uuid {
        match self {
            Self::Allocate(p) => p.operation_id,
            Self::Create(p) => p.operation_id,
            Self::Release(p) => p.operation_id,
        }
    }
    fn age(&self) -> u64 {
        match self {
            Self::Allocate(p) => p.issued_ms,
            Self::Create(p) => p.issued_ms,
            Self::Release(p) => p.issued_ms,
        }
    }
    fn generation(&self) -> &str {
        match self {
            Self::Allocate(p) => &p.generation,
            Self::Create(p) => &p.generation,
            Self::Release(p) => &p.generation,
        }
    }
    fn fingerprint(&self) -> Vec<u8> {
        let (method, params) = match self {
            Self::Allocate(p) => ("allocate_ip", serde_json::to_value(p).unwrap()),
            Self::Create(p) => ("create_udp", serde_json::to_value(p).unwrap()),
            Self::Release(p) => ("release_udp", serde_json::to_value(p).unwrap()),
        };
        serde_json::to_vec(&(method, params)).unwrap()
    }
}
struct Entry {
    fingerprint: Vec<u8>,
    expires: u64,
    result: Result<Value, &'static str>,
    allocation: Option<Uuid>,
}
pub struct Operations {
    entries: BTreeMap<(Uuid, Uuid), Entry>,
    limit: usize,
}
impl Operations {
    pub fn new(limit: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            limit,
        }
    }
    pub fn execute(
        &mut self,
        op: &Operation,
        generation: &str,
        now: u64,
        run: impl FnOnce() -> (Result<Value, &'static str>, Option<Uuid>),
    ) -> Result<Value, &'static str> {
        self.entries.retain(|_, e| now < e.expires);
        if op.generation() != generation {
            return Err("stale_generation");
        }
        if op.age() > now || now - op.age() >= RETRY_MS {
            return Err("expired_operation");
        }
        let key = (op.client(), op.id());
        let fingerprint = op.fingerprint();
        if let Some(entry) = self.entries.get(&key) {
            return if entry.fingerprint == fingerprint {
                entry.result.clone()
            } else {
                Err("idempotency_conflict")
            };
        }
        if self.entries.len() >= self.limit {
            return Err("capacity");
        }
        let (result, allocation) = run();
        self.entries.insert(
            key,
            Entry {
                fingerprint,
                expires: op.age().saturating_add(RETRY_MS),
                result: result.clone(),
                allocation,
            },
        );
        result
    }
    pub fn terminal(&mut self, allocation: Uuid) {
        for entry in self.entries.values_mut() {
            if entry.allocation == Some(allocation) {
                entry.result = Err("allocation_terminal");
                entry.allocation = None;
            }
        }
    }
}
pub fn error_code(message: &str) -> i32 {
    match message {
        "unknown_method" => -32601,
        "invalid_params" => -32602,
        "stale_generation" => -32001,
        "idempotency_conflict" => -32004,
        "address_conflict" => -32005,
        "pool_exhausted" => -32006,
        "allocation_quota" => -32007,
        "port_pool_exhausted" => -32008,
        "firewall_unavailable" => -32009,
        "capacity" => -32011,
        "expired_operation" => -32012,
        "allocation_terminal" => -32013,
        "unsupported_address_family" => -32014,
        _ => -32010,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn retries_conflicts_terminal_horizon_pressure_and_generation() {
        let op = Operation::Create(Create {
            client_id: Uuid::new_v4(),
            operation_id: Uuid::new_v4(),
            generation: "boot".into(),
            issued_ms: 0,
            inner: "10.0.0.2".parse().unwrap(),
        });
        let mut cache = Operations::new(1);
        let allocation = Uuid::new_v4();
        assert_eq!(
            cache
                .execute(&op, "boot", 0, || {
                    (
                        Ok(json!({"port":123,"secret":"test secret"})),
                        Some(allocation),
                    )
                })
                .unwrap()["port"],
            123
        );
        assert_eq!(
            cache.execute(&op, "boot", 1, || panic!()).unwrap()["port"],
            123
        );
        let mut changed = op.clone();
        if let Operation::Create(p) = &mut changed {
            p.inner = "10.0.0.3".parse().unwrap();
        }
        assert_eq!(
            cache.execute(&changed, "boot", 2, || panic!()),
            Err("idempotency_conflict")
        );
        if let Operation::Create(p) = &mut changed {
            p.operation_id = Uuid::new_v4();
        }
        assert_eq!(
            cache.execute(&changed, "boot", 3, || panic!()),
            Err("capacity")
        );
        cache.terminal(allocation);
        assert_eq!(
            cache.execute(&op, "boot", 4, || panic!()),
            Err("allocation_terminal")
        );
        assert_eq!(
            cache.execute(&op, "boot", RETRY_MS, || panic!()),
            Err("expired_operation")
        );
        assert_eq!(
            cache.execute(&op, "new", 0, || panic!()),
            Err("stale_generation")
        );
    }
}
