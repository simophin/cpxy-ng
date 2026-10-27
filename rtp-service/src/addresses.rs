//! Stable UUID/address ownership. No UDP keys or replay state are persisted.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io,
    net::Ipv4Addr,
    path::PathBuf,
};
use uuid::Uuid;

use rtp_l3::v2::identity::lock;
pub use rtp_l3::v2::identity::{atomic_save, client_id};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u8,
    server: Ipv4Addr,
    prefix: u8,
    mappings: BTreeMap<Uuid, Ipv4Addr>,
}
pub struct Addresses {
    path: PathBuf,
    _lock: File,
    stored: Stored,
    low: u32,
    high: u32,
    reserved: BTreeSet<Ipv4Addr>,
}
impl Addresses {
    pub fn open(
        path: PathBuf,
        server: Ipv4Addr,
        prefix: u8,
        low: Ipv4Addr,
        high: Ipv4Addr,
        reserved: Vec<Ipv4Addr>,
    ) -> io::Result<Self> {
        if prefix > 30 {
            return Err(io::Error::other(
                "tunnel subnet requires usable client addresses",
            ));
        }
        let _lock = lock(&path)?;
        let stored = match fs::read(&path) {
            Ok(b) => serde_json::from_slice::<Stored>(&b)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Stored {
                version: 2,
                server,
                prefix,
                mappings: BTreeMap::new(),
            },
            Err(e) => return Err(e),
        };
        if stored.version != 2 || stored.server != server || stored.prefix != prefix {
            return Err(io::Error::other(
                "address state subnet differs from configuration",
            ));
        }
        let this = Self {
            path,
            _lock,
            stored,
            low: u32::from(low),
            high: u32::from(high),
            reserved: reserved.into_iter().collect(),
        };
        if this.low > this.high
            || !this.usable(low)
            || !this.usable(high)
            || this.high - this.low > 1_048_575
        {
            return Err(io::Error::other(
                "invalid dynamic address pool (maximum 1048576 addresses)",
            ));
        }
        let mut seen = BTreeSet::new();
        if this
            .stored
            .mappings
            .iter()
            .any(|(id, ip)| id.is_nil() || !this.usable(*ip) || !seen.insert(*ip))
            || this.reserved.iter().any(|ip| !this.usable(*ip))
        {
            return Err(io::Error::other(
                "invalid or duplicate persisted address mapping/reservation",
            ));
        }
        Ok(this)
    }
    pub fn usable(&self, ip: Ipv4Addr) -> bool {
        let mask = if self.stored.prefix == 0 {
            0
        } else {
            u32::MAX << (32 - self.stored.prefix)
        };
        let network = u32::from(self.stored.server) & mask;
        let value = u32::from(ip);
        value & mask == network
            && value != network
            && value != network | !mask
            && ip != self.stored.server
    }
    pub fn get(&self, client: Uuid) -> Option<Ipv4Addr> {
        self.stored.mappings.get(&client).copied()
    }
    pub fn owner(&self, ip: Ipv4Addr) -> Option<Uuid> {
        self.stored
            .mappings
            .iter()
            .find_map(|(id, address)| (*address == ip).then_some(*id))
    }
    pub fn allocate(&mut self, client: Uuid) -> Result<Ipv4Addr, &'static str> {
        if let Some(ip) = self.get(client) {
            return Ok(ip);
        }
        let used: BTreeSet<_> = self.stored.mappings.values().copied().collect();
        let ip = (self.low..=self.high)
            .map(Ipv4Addr::from)
            .find(|ip| self.usable(*ip) && !self.reserved.contains(ip) && !used.contains(ip))
            .ok_or("pool_exhausted")?;
        self.claim(client, ip)
    }
    pub fn claim(&mut self, client: Uuid, ip: Ipv4Addr) -> Result<Ipv4Addr, &'static str> {
        if client.is_nil() || !self.usable(ip) {
            return Err("invalid_params");
        }
        if self.get(client).is_some_and(|old| old != ip)
            || self.owner(ip).is_some_and(|old| old != client)
        {
            return Err("address_conflict");
        }
        if self.get(client) == Some(ip) {
            return Ok(ip);
        }
        let mut proposed = self.stored.clone();
        proposed.mappings.insert(client, ip);
        atomic_save(
            &self.path,
            &serde_json::to_vec(&proposed).map_err(|_| "temporary_failure")?,
        )
        .map_err(|_| "temporary_failure")?;
        self.stored = proposed;
        Ok(ip)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistence_reservations_static_collision_lock_and_exhaustion() {
        let root = std::env::temp_dir().join(format!("rtp-address-test-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("addresses.json");
        let open = || {
            Addresses::open(
                path.clone(),
                "10.0.0.1".parse().unwrap(),
                29,
                "10.0.0.2".parse().unwrap(),
                "10.0.0.4".parse().unwrap(),
                vec!["10.0.0.3".parse().unwrap()],
            )
        };
        let mut a = open().unwrap();
        assert!(open().is_err());
        let c = Uuid::new_v4();
        let d = Uuid::new_v4();
        let e = Uuid::new_v4();
        assert_eq!(
            a.claim(c, "10.0.0.2".parse().unwrap()).unwrap().to_string(),
            "10.0.0.2"
        );
        assert_eq!(a.allocate(d).unwrap().to_string(), "10.0.0.4");
        assert_eq!(a.allocate(e), Err("pool_exhausted"));
        assert_eq!(
            a.claim(e, "10.0.0.2".parse().unwrap()),
            Err("address_conflict")
        );
        assert_eq!(
            a.claim(c, "10.0.0.5".parse().unwrap()),
            Err("address_conflict")
        );
        for invalid in ["10.0.0.0", "10.0.0.1", "10.0.0.7", "10.0.1.2"] {
            assert_eq!(a.claim(e, invalid.parse().unwrap()), Err("invalid_params"));
        }
        drop(a);
        let mut a = open().unwrap();
        assert_eq!(a.allocate(c).unwrap().to_string(), "10.0.0.2");
        drop(a);
        let id_path = root.join("identity");
        assert_eq!(client_id(&id_path).unwrap(), client_id(&id_path).unwrap());
        fs::write(&id_path, "broken").unwrap();
        assert!(client_id(&id_path).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
