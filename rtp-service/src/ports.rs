//! Protocol-agnostic admission contract and durable generation-bound inventory.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    net::SocketAddr,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Mutex,
};

/// A2 implements this trait with a privileged firewalld agent. No RTP keys or
/// packets cross this boundary. Calls must be bounded by the adapter.
pub trait Admission: Send + Sync {
    fn prepare_port(&self, lease: &Lease) -> io::Result<()>;
    fn commit_port(&self, lease: &Lease) -> io::Result<()>;
    fn retire_port(&self, lease: &Lease) -> io::Result<()>;
    fn reconcile_ports(&self, leases: &[Lease]) -> io::Result<()>;
}
pub struct AdministratorManaged;
impl Admission for AdministratorManaged {
    fn prepare_port(&self, _: &Lease) -> io::Result<()> {
        Ok(())
    }
    fn commit_port(&self, _: &Lease) -> io::Result<()> {
        Ok(())
    }
    fn retire_port(&self, _: &Lease) -> io::Result<()> {
        Ok(())
    }
    fn reconcile_ports(&self, _: &[Lease]) -> io::Result<()> {
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    Preparing,
    Prepared,
    Active,
    Retiring,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lease {
    pub profile: String,
    pub generation: String,
    pub address: SocketAddr,
    pub state: LeaseState,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u32,
    leases: Vec<Lease>,
}
pub struct LeaseManager {
    path: PathBuf,
    _lock: File,
    profile: String,
    generation: String,
    admission: Box<dyn Admission>,
    leases: Mutex<BTreeMap<u16, Lease>>,
}
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}
fn save(path: &Path, leases: &BTreeMap<u16, Lease>) -> io::Result<()> {
    let temporary = sibling(path, ".new");
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    f.write_all(&serde_json::to_vec(&Journal {
        version: 1,
        leases: leases.values().cloned().collect(),
    })?)?;
    f.sync_all()?;
    fs::rename(&temporary, path)?;
    File::open(
        path.parent()
            .ok_or_else(|| io::Error::other("missing journal directory"))?,
    )?
    .sync_all()
}
impl LeaseManager {
    pub fn open(path: PathBuf, profile: String, admission: Box<dyn Admission>) -> io::Result<Self> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(sibling(&path, ".lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::other("profile journal already in use"));
        }
        let mut leases = BTreeMap::new();
        match fs::read(&path) {
            Ok(bytes) => {
                let journal: Journal = serde_json::from_slice(&bytes)?;
                if journal.version != 1 {
                    return Err(io::Error::other("unsupported journal version"));
                }
                for lease in journal.leases {
                    if lease.profile != profile
                        || leases.insert(lease.address.port(), lease).is_some()
                    {
                        return Err(io::Error::other("journal ownership conflict"));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Under exclusive lock stale generations cannot still be serving. Keep
        // failed retirements journaled so a subsequent cleanup can retry.
        for port in leases.keys().copied().collect::<Vec<_>>() {
            admission.retire_port(&leases[&port])?;
            leases.remove(&port);
            save(&path, &leases)?;
        }
        let generation = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos()
        );
        save(&path, &leases)?;
        Ok(Self {
            path,
            _lock: lock,
            profile,
            generation,
            admission,
            leases: Mutex::new(leases),
        })
    }
    pub fn prepare_port(&self, address: SocketAddr) -> io::Result<()> {
        let mut leases = self.leases.lock().unwrap();
        if leases.contains_key(&address.port()) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "port already leased",
            ));
        }
        let mut lease = Lease {
            profile: self.profile.clone(),
            generation: self.generation.clone(),
            address,
            state: LeaseState::Preparing,
        };
        leases.insert(address.port(), lease.clone());
        save(&self.path, &leases)?;
        // Intent survives ambiguous failures. Supervisor cleanup/restart retires it.
        self.admission.prepare_port(&lease)?;
        lease.state = LeaseState::Prepared;
        leases.insert(address.port(), lease);
        save(&self.path, &leases)
    }
    pub fn commit_port(&self, port: u16) -> io::Result<()> {
        let mut leases = self.leases.lock().unwrap();
        let lease = leases
            .get_mut(&port)
            .ok_or_else(|| io::Error::other("unknown lease"))?;
        self.admission.commit_port(lease)?;
        lease.state = LeaseState::Active;
        save(&self.path, &leases)
    }
    pub fn retire_port(&self, port: u16) -> io::Result<()> {
        let mut leases = self.leases.lock().unwrap();
        let Some(lease) = leases.get_mut(&port) else {
            return Ok(());
        };
        lease.state = LeaseState::Retiring;
        let lease = lease.clone();
        save(&self.path, &leases)?;
        self.admission.retire_port(&lease)?;
        leases.remove(&port);
        save(&self.path, &leases)
    }
    pub fn reconcile_ports(&self) -> io::Result<()> {
        self.admission.reconcile_ports(
            &self
                .leases
                .lock()
                .unwrap()
                .values()
                .cloned()
                .collect::<Vec<_>>(),
        )
    }
    pub fn cleanup(&self) -> io::Result<()> {
        let ports: Vec<_> = self.leases.lock().unwrap().keys().copied().collect();
        let mut result = Ok(());
        for port in ports {
            if let Err(e) = self.retire_port(port) {
                result = Err(e);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "rtp-journal-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path.join("leases.json"))
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.parent().unwrap());
        }
    }
    struct Fake {
        fail_prepare: bool,
        fail_commit: bool,
        fail_retire: Arc<AtomicBool>,
        retired: Arc<Mutex<Vec<String>>>,
    }
    impl Admission for Fake {
        fn prepare_port(&self, _: &Lease) -> io::Result<()> {
            if self.fail_prepare {
                Err(io::Error::other("ambiguous prepare"))
            } else {
                Ok(())
            }
        }
        fn commit_port(&self, _: &Lease) -> io::Result<()> {
            if self.fail_commit {
                Err(io::Error::other("commit failed"))
            } else {
                Ok(())
            }
        }
        fn retire_port(&self, lease: &Lease) -> io::Result<()> {
            if self.fail_retire.load(Ordering::Relaxed) {
                return Err(io::Error::other("retire failed"));
            }
            self.retired.lock().unwrap().push(lease.generation.clone());
            Ok(())
        }
        fn reconcile_ports(&self, _: &[Lease]) -> io::Result<()> {
            Ok(())
        }
    }
    fn fake(
        fail_prepare: bool,
        fail_commit: bool,
        fail_retire: Arc<AtomicBool>,
        retired: Arc<Mutex<Vec<String>>>,
    ) -> Box<dyn Admission> {
        Box::new(Fake {
            fail_prepare,
            fail_commit,
            fail_retire,
            retired,
        })
    }
    fn inventory(path: &Path) -> Vec<Lease> {
        serde_json::from_slice::<Journal>(&fs::read(path).unwrap())
            .unwrap()
            .leases
    }
    #[test]
    fn failed_prepare_and_commit_remain_durable_until_cleanup() {
        for (prepare, commit) in [(true, false), (false, true)] {
            let temp = Temp::new();
            let manager = LeaseManager::open(
                temp.0.clone(),
                "test".into(),
                fake(
                    prepare,
                    commit,
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(Mutex::new(vec![])),
                ),
            )
            .unwrap();
            let result = manager.prepare_port("127.0.0.1:30100".parse().unwrap());
            if prepare {
                assert!(result.is_err());
            } else {
                result.unwrap();
                assert!(manager.commit_port(30100).is_err());
            }
            assert_eq!(inventory(&temp.0).len(), 1);
            manager.cleanup().unwrap();
            manager.retire_port(30100).unwrap();
            assert!(inventory(&temp.0).is_empty());
        }
    }
    #[test]
    fn restart_retires_exact_stale_generation_and_lock_excludes_cleanup() {
        let temp = Temp::new();
        let retired = Arc::new(Mutex::new(vec![]));
        let failure = Arc::new(AtomicBool::new(false));
        let manager = LeaseManager::open(
            temp.0.clone(),
            "test".into(),
            fake(false, false, failure.clone(), retired.clone()),
        )
        .unwrap();
        manager
            .prepare_port("127.0.0.1:30100".parse().unwrap())
            .unwrap();
        manager.commit_port(30100).unwrap();
        let old_generation = inventory(&temp.0)[0].generation.clone();
        assert!(LeaseManager::open(
            temp.0.clone(),
            "test".into(),
            Box::new(AdministratorManaged)
        )
        .is_err());
        drop(manager); // Simulate supervisor death: journal deliberately survives.
        let manager = LeaseManager::open(
            temp.0.clone(),
            "test".into(),
            fake(false, false, failure, retired.clone()),
        )
        .unwrap();
        assert_eq!(*retired.lock().unwrap(), vec![old_generation.clone()]);
        assert!(inventory(&temp.0).is_empty());
        manager
            .prepare_port("127.0.0.1:30100".parse().unwrap())
            .unwrap();
        assert_ne!(inventory(&temp.0)[0].generation, old_generation);
    }
    #[test]
    fn failed_retirement_survives_restart_and_wrong_profile_is_rejected() {
        let temp = Temp::new();
        let failure = Arc::new(AtomicBool::new(true));
        let retired = Arc::new(Mutex::new(vec![]));
        let manager = LeaseManager::open(
            temp.0.clone(),
            "test".into(),
            fake(false, false, failure.clone(), retired.clone()),
        )
        .unwrap();
        manager
            .prepare_port("127.0.0.1:30100".parse().unwrap())
            .unwrap();
        assert!(manager.cleanup().is_err());
        assert_eq!(inventory(&temp.0)[0].state, LeaseState::Retiring);
        drop(manager);
        assert!(LeaseManager::open(
            temp.0.clone(),
            "other".into(),
            Box::new(AdministratorManaged)
        )
        .is_err());
        assert!(LeaseManager::open(
            temp.0.clone(),
            "test".into(),
            fake(false, false, failure.clone(), retired.clone())
        )
        .is_err());
        assert_eq!(inventory(&temp.0).len(), 1);
        failure.store(false, Ordering::Relaxed);
        LeaseManager::open(
            temp.0.clone(),
            "test".into(),
            fake(false, false, failure, retired),
        )
        .unwrap();
        assert!(inventory(&temp.0).is_empty());
    }
}
