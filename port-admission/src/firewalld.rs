//! Runtime rich-rule ownership. All subprocesses speak D-Bus directly; no shell
//! or firewall-cmd is used, and each call has a one-second D-Bus deadline.
use crate::{Admission, Lease};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap, fs, io, net::Ipv4Addr, path::PathBuf, process::Command, sync::Mutex,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub profile: String,
    pub bind: Ipv4Addr,
    pub base: u16,
    pub low: u16,
    pub high: u16,
    pub zone: String,
    pub interface: String,
    pub sources: Vec<String>,
}
impl Policy {
    pub fn validate(&self) -> io::Result<()> {
        let safe = |s: &str| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        };
        if !safe(&self.profile)
            || !safe(&self.zone)
            || !safe(&self.interface)
            || self.base == 0
            || self.low == 0
            || self.low > self.high
            || self.sources.is_empty()
            || self.sources.len() > 4
        {
            return Err(io::Error::other("invalid delegated firewall policy"));
        }
        for source in &self.sources {
            let (ip, prefix) = source
                .split_once('/')
                .ok_or_else(|| io::Error::other("source needs IPv4 CIDR"))?;
            let _: Ipv4Addr = ip.parse().map_err(io::Error::other)?;
            let prefix: u8 = prefix.parse().map_err(io::Error::other)?;
            if prefix > 32 {
                return Err(io::Error::other("invalid source prefix"));
            }
        }
        Ok(())
    }
    fn rules(&self, lease: &Lease) -> io::Result<Vec<String>> {
        self.validate()?;
        if lease.profile != self.profile
            || lease.address.ip() != self.bind
            || (lease.address.port() != self.base
                && !(self.low..=self.high).contains(&lease.address.port()))
            || lease.generation.is_empty()
            || lease.generation.len() > 128
            || !lease
                .generation
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "lease outside delegated policy",
            ));
        }
        Ok(self.sources.iter().map(|source| format!("rule family=\"ipv4\" source address=\"{source}\" {}port port=\"{}\" protocol=\"udp\" accept", if self.bind.is_unspecified() { String::new() } else { format!("destination address=\"{}\" ", self.bind) }, lease.address.port())).collect())
    }
}

pub trait Firewall: Send + Sync {
    fn zone(&self, interface: &str) -> io::Result<String>;
    fn query(&self, zone: &str, rule: &str) -> io::Result<bool>;
    fn add(&self, zone: &str, rule: &str) -> io::Result<()>;
    fn remove(&self, zone: &str, rule: &str) -> io::Result<()>;
}
pub struct DbusFirewall;
impl DbusFirewall {
    fn call(method: &str, signature: &str, args: &[&str]) -> io::Result<String> {
        Self::call_on("org.fedoraproject.FirewallD1.zone", method, signature, args)
    }
    fn call_on(
        interface: &str,
        method: &str,
        signature: &str,
        args: &[&str],
    ) -> io::Result<String> {
        let mut command = Command::new("busctl");
        if let Ok(address) = std::env::var("DBUS_SYSTEM_BUS_ADDRESS") {
            command.arg(format!("--address={address}"));
        } else {
            command.arg("--system");
        }
        let output = command
            .args([
                "--timeout=1s",
                "call",
                "org.fedoraproject.FirewallD1",
                "/org/fedoraproject/FirewallD1",
                interface,
                method,
                signature,
            ])
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "firewalld {method}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}
fn string_reply(value: &str) -> io::Result<String> {
    value
        .strip_prefix("s ")
        .and_then(|s| serde_json::from_str(s).ok())
        .ok_or_else(|| io::Error::other("invalid D-Bus string reply"))
}
impl DbusFirewall {
    /// True when firewalld answers on the system bus. A missing busctl or bus
    /// means firewalld is absent; callers treat that as a soft dependency.
    pub fn available() -> bool {
        Self::call_on("org.fedoraproject.FirewallD1", "getDefaultZone", "", &[]).is_ok()
    }
    /// Runtime zone binding of an interface; empty when unassigned.
    pub fn interface_zone(interface: &str) -> io::Result<String> {
        string_reply(&Self::call("getZoneOfInterface", "s", &[interface])?)
    }
    /// Runtime-only binding; never touches permanent configuration.
    pub fn bind_interface(zone: &str, interface: &str) -> io::Result<()> {
        Self::call("addInterface", "ss", &[zone, interface]).map(|_| ())
    }
    pub fn unbind_interface(zone: &str, interface: &str) -> io::Result<()> {
        Self::call("removeInterface", "ss", &[zone, interface]).map(|_| ())
    }
}
impl Firewall for DbusFirewall {
    fn zone(&self, interface: &str) -> io::Result<String> {
        let value = Self::call("getZoneOfInterface", "s", &[interface])?;
        let mut zone: String = value
            .strip_prefix("s ")
            .and_then(|s| serde_json::from_str(s).ok())
            .ok_or_else(|| io::Error::other("invalid D-Bus zone reply"))?;
        if zone.is_empty() {
            let mut command = Command::new("busctl");
            if let Ok(address) = std::env::var("DBUS_SYSTEM_BUS_ADDRESS") {
                command.arg(format!("--address={address}"));
            } else {
                command.arg("--system");
            }
            let output = command
                .args([
                    "--timeout=1s",
                    "call",
                    "org.fedoraproject.FirewallD1",
                    "/org/fedoraproject/FirewallD1",
                    "org.fedoraproject.FirewallD1",
                    "getDefaultZone",
                ])
                .output()?;
            if !output.status.success() {
                return Err(io::Error::other("cannot resolve default zone"));
            }
            zone = String::from_utf8_lossy(&output.stdout)
                .trim()
                .strip_prefix("s ")
                .and_then(|s| serde_json::from_str(s).ok())
                .ok_or_else(|| io::Error::other("invalid default zone reply"))?;
        }
        Ok(zone)
    }
    fn query(&self, zone: &str, rule: &str) -> io::Result<bool> {
        match Self::call("queryRichRule", "ss", &[zone, rule])?.as_str() {
            "b true" => Ok(true),
            "b false" => Ok(false),
            _ => Err(io::Error::other("invalid D-Bus query reply")),
        }
    }
    fn add(&self, zone: &str, rule: &str) -> io::Result<()> {
        Self::call("addRichRule", "ssi", &[zone, rule, "0"]).map(|_| ())
    }
    fn remove(&self, zone: &str, rule: &str) -> io::Result<()> {
        Self::call("removeRichRule", "ss", &[zone, rule]).map(|_| ())
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Claim {
    lease: Lease,
    zone: String,
    rules: Vec<String>,
}
/// The agent journal is separate from each service journal and holds exact rules.
/// An exclusive lock serializes profiles and prevents simultaneous old agents.
pub struct ManagedFirewall<F: Firewall> {
    policies: Vec<Policy>,
    firewall: F,
    path: PathBuf,
    _lock: fs::File,
    claims: Mutex<BTreeMap<String, Claim>>,
}
fn claim_key(lease: &Lease) -> String {
    format!("{}:{}", lease.profile, lease.address)
}
impl<F: Firewall> ManagedFirewall<F> {
    pub fn open(policies: Vec<Policy>, firewall: F, path: PathBuf) -> io::Result<Self> {
        use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
        if policies.is_empty() || policies.len() > 32 {
            return Err(io::Error::other("policy count must be 1..32"));
        }
        for policy in &policies {
            policy.validate()?;
        }
        for (i, a) in policies.iter().enumerate() {
            for b in &policies[..i] {
                if a.profile == b.profile
                    || ((a.bind == b.bind || a.bind.is_unspecified() || b.bind.is_unspecified())
                        && (a.base == b.base
                            || (b.low..=b.high).contains(&a.base)
                            || (a.low..=a.high).contains(&b.base)
                            || a.low <= b.high && b.low <= a.high))
                {
                    return Err(io::Error::other("overlapping delegated namespaces"));
                }
            }
        }
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(crate::sibling(&path, ".lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::other("firewall agent already running"));
        }
        let claims = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        let this = Self {
            policies,
            firewall,
            path,
            _lock: lock,
            claims: Mutex::new(claims),
        };
        // Validate persisted canonical rules before permitting any destructive call.
        for claim in this.claims.lock().unwrap().values() {
            if this.rules(&claim.lease)? != claim.rules
                || this.policy(&claim.lease)?.zone != claim.zone
            {
                return Err(io::Error::other("agent journal policy mismatch"));
            }
        }
        Ok(this)
    }
    fn policy(&self, lease: &Lease) -> io::Result<&Policy> {
        self.policies
            .iter()
            .find(|p| p.profile == lease.profile)
            .ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "unknown profile"))
    }
    fn rules(&self, lease: &Lease) -> io::Result<Vec<String>> {
        self.policy(lease)?.rules(lease)
    }
    fn save(&self, claims: &BTreeMap<String, Claim>) -> io::Result<()> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let temp = crate::sibling(&self.path, ".new");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec(claims)?)?;
        file.sync_all()?;
        fs::rename(temp, &self.path)?;
        fs::File::open(
            self.path
                .parent()
                .ok_or_else(|| io::Error::other("journal directory"))?,
        )?
        .sync_all()
    }
    fn verify(&self, lease: &Lease, restore: bool) -> io::Result<()> {
        let policy = self.policy(lease)?;
        if self.firewall.zone(&policy.interface)? != policy.zone {
            return Err(io::Error::other("firewall interface/zone mismatch"));
        }
        let claims = self.claims.lock().unwrap();
        let claim = claims
            .get(&claim_key(lease))
            .filter(|c| c.lease.generation == lease.generation)
            .ok_or_else(|| io::Error::other("stale or unknown generation"))?;
        for rule in &claim.rules {
            if !self.firewall.query(&claim.zone, rule)? {
                if !restore {
                    return Err(io::Error::other("owned rule absent"));
                }
                self.firewall.add(&claim.zone, rule)?;
                if !self.firewall.query(&claim.zone, rule)? {
                    return Err(io::Error::other("restored rule query failed"));
                }
            }
        }
        Ok(())
    }
}
impl<F: Firewall> Admission for ManagedFirewall<F> {
    fn prepare_port(&self, lease: &Lease) -> io::Result<()> {
        let rules = self.rules(lease)?;
        let policy = self.policy(lease)?;
        if self.firewall.zone(&policy.interface)? != policy.zone {
            return Err(io::Error::other("firewall interface/zone mismatch"));
        }
        let mut claims = self.claims.lock().unwrap();
        let key = claim_key(lease);
        if let Some(old) = claims.get(&key) {
            if old.lease.generation != lease.generation {
                return Err(io::Error::other("port owned by another generation"));
            }
            drop(claims);
            return self.verify(lease, true);
        }
        for rule in &rules {
            if self.firewall.query(&policy.zone, rule)? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "preexisting rule is not owned",
                ));
            }
        }
        claims.insert(
            key,
            Claim {
                lease: lease.clone(),
                zone: policy.zone.clone(),
                rules: rules.clone(),
            },
        );
        self.save(&claims)?; // Intent durable before the first mutation.
        drop(claims);
        for rule in &rules {
            self.firewall.add(&policy.zone, rule)?;
        }
        self.verify(lease, false)
    }
    fn commit_port(&self, lease: &Lease) -> io::Result<()> {
        self.verify(lease, false)
    }
    fn retire_port(&self, lease: &Lease) -> io::Result<()> {
        self.rules(lease)?;
        let mut claims = self.claims.lock().unwrap();
        let key = claim_key(lease);
        let Some(claim) = claims.get(&key) else {
            return Ok(());
        };
        if claim.lease.generation != lease.generation {
            return Err(io::Error::other("stale retirement generation"));
        }
        for rule in &claim.rules {
            if self.firewall.query(&claim.zone, rule)? {
                self.firewall.remove(&claim.zone, rule)?;
            }
            if self.firewall.query(&claim.zone, rule)? {
                return Err(io::Error::other("removed rule remains"));
            }
        }
        claims.remove(&key);
        self.save(&claims)
    }
    fn reconcile_ports(&self, leases: &[Lease]) -> io::Result<()> {
        for lease in leases {
            self.verify(lease, true)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LeaseState;
    use std::{
        collections::BTreeSet,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };
    #[derive(Default)]
    struct State {
        rules: BTreeSet<String>,
        fail: Option<&'static str>,
        mutations: usize,
    }
    #[derive(Clone, Default)]
    struct Fake(Arc<Mutex<State>>);
    impl Firewall for Fake {
        fn zone(&self, _: &str) -> io::Result<String> {
            Ok("public".into())
        }
        fn query(&self, _: &str, rule: &str) -> io::Result<bool> {
            let state = self.0.lock().unwrap();
            if state.fail == Some("query") {
                return Err(io::Error::other("query timeout"));
            }
            Ok(state.rules.contains(rule))
        }
        fn add(&self, _: &str, rule: &str) -> io::Result<()> {
            let mut state = self.0.lock().unwrap();
            if state.fail == Some("add") {
                return Err(io::Error::other("add failed"));
            }
            state.rules.insert(rule.into());
            state.mutations += 1;
            if state.fail == Some("ambiguous") {
                return Err(io::Error::other("reply lost after add"));
            }
            Ok(())
        }
        fn remove(&self, _: &str, rule: &str) -> io::Result<()> {
            let mut state = self.0.lock().unwrap();
            if state.fail == Some("remove") {
                return Err(io::Error::other("remove failed"));
            }
            state.rules.remove(rule);
            state.mutations += 1;
            Ok(())
        }
    }
    fn policy() -> Policy {
        Policy {
            profile: "one".into(),
            bind: "127.0.0.1".parse().unwrap(),
            base: 31000,
            low: 31001,
            high: 31005,
            zone: "public".into(),
            interface: "lo".into(),
            sources: vec!["127.0.0.0/8".into()],
        }
    }
    fn lease(generation: &str) -> Lease {
        Lease {
            profile: "one".into(),
            generation: generation.into(),
            address: "127.0.0.1:31001".parse().unwrap(),
            state: LeaseState::Preparing,
        }
    }
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture {
        dir: PathBuf,
        fake: Fake,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "firewall-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            Self {
                dir,
                fake: Fake::default(),
            }
        }
        fn open(&self) -> ManagedFirewall<Fake> {
            ManagedFirewall::open(
                vec![policy()],
                self.fake.clone(),
                self.dir.join("claims.json"),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
    #[test]
    fn preexisting_and_failed_query_are_never_claimed_or_removed() {
        for failure in [None, Some("query")] {
            let fixture = Fixture::new();
            let agent = fixture.open();
            let lease = lease("old");
            let rule = policy().rules(&lease).unwrap().remove(0);
            fixture.fake.0.lock().unwrap().rules.insert(rule.clone());
            fixture.fake.0.lock().unwrap().fail = failure;
            assert!(agent.prepare_port(&lease).is_err());
            fixture.fake.0.lock().unwrap().fail = None;
            agent.retire_port(&lease).unwrap();
            assert!(fixture.fake.0.lock().unwrap().rules.contains(&rule));
            assert_eq!(fixture.fake.0.lock().unwrap().mutations, 0);
        }
    }
    #[test]
    fn failures_ambiguous_add_restart_and_idempotent_release() {
        for failure in ["add", "ambiguous", "query", "remove"] {
            let fixture = Fixture::new();
            let agent = fixture.open();
            let lease = lease("old");
            if failure == "remove" {
                agent.prepare_port(&lease).unwrap();
            }
            fixture.fake.0.lock().unwrap().fail = Some(failure);
            if failure == "remove" {
                assert!(agent.retire_port(&lease).is_err());
            } else {
                assert!(agent.prepare_port(&lease).is_err());
            }
            drop(agent);
            fixture.fake.0.lock().unwrap().fail = None;
            let agent = fixture.open();
            agent.retire_port(&lease).unwrap();
            agent.retire_port(&lease).unwrap();
            assert!(fixture.fake.0.lock().unwrap().rules.is_empty());
        }
    }
    #[test]
    fn stale_generation_policy_reuse_and_reload() {
        let fixture = Fixture::new();
        let agent = fixture.open();
        let old = lease("old");
        let new = lease("new");
        agent.prepare_port(&old).unwrap();
        assert!(agent.prepare_port(&new).is_err());
        fixture.fake.0.lock().unwrap().rules.clear();
        assert!(agent.commit_port(&old).is_err());
        agent.reconcile_ports(&[old.clone()]).unwrap();
        agent.commit_port(&old).unwrap();
        agent.retire_port(&old).unwrap();
        agent.prepare_port(&new).unwrap();
        assert!(agent.retire_port(&old).is_err());
        agent.commit_port(&new).unwrap();
        let mut invalid = new.clone();
        invalid.address.set_port(40000);
        assert!(agent.prepare_port(&invalid).is_err());
        agent.retire_port(&new).unwrap();
    }
}
