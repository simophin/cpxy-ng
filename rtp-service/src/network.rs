//! Explicit IPv4 service policy. Dedicated routing tables remain fail closed
//! after process death; only explicit stop/cleanup releases their guards.
use crate::config::{Role, Settings};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    net::Ipv4Addr,
    path::PathBuf,
    process::Command,
};
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// Full IPv4 exit. Clients leave route selection to the gateway administrator
    /// so forwarded networks can use RTP without capturing router-local traffic.
    #[serde(default)]
    pub internet_exit: bool,
    pub tunnel_pool: String,
    pub client_addresses: Vec<Ipv4Addr>,
    pub service_cidrs: Vec<String>,
    /// Reserved exclusively for this profile, including its fail-closed guard.
    pub table: u32,
    pub rule_priority: u32,
    #[serde(default)]
    pub dns: Option<Dns>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Dns {
    pub mode: DnsMode,
    pub resolver: Ipv4Addr,
    pub suffixes: Vec<String>,
}
/// Metadata only: administrator must provision a persistent, fail-closed local
/// resolver that never retries private suffixes through public resolvers.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DnsMode {
    ExternallyManagedFailClosed,
}
fn bad(s: &str) -> io::Error {
    io::Error::other(s)
}
fn cidr(s: &str) -> io::Result<(u32, u8)> {
    let (a, b) = s.split_once('/').ok_or_else(|| bad("CIDR needs prefix"))?;
    let a = u32::from(
        a.parse::<Ipv4Addr>()
            .map_err(|_| bad("invalid IPv4 CIDR"))?,
    );
    let b = b.parse::<u8>().map_err(|_| bad("invalid CIDR prefix"))?;
    if b > 32 || a & !mask(b) != 0 {
        return Err(bad("CIDR must be canonical"));
    }
    Ok((a, b))
}
fn mask(n: u8) -> u32 {
    if n == 0 {
        0
    } else {
        u32::MAX << (32 - n)
    }
}
fn contains(c: &str, a: Ipv4Addr) -> bool {
    cidr(c).is_ok_and(|(v, n)| u32::from(a) & mask(n) == v)
}
impl Network {
    pub fn validate(&self, s: &Settings) -> io::Result<()> {
        let (pool, prefix) = cidr(&self.tunnel_pool)?;
        let (inner, inner_prefix) = s.inner()?;
        if inner_prefix != prefix || u32::from(inner) & mask(prefix) != pool {
            return Err(bad("TUN address prefix must match tunnel_pool"));
        }
        if self
            .client_addresses
            .iter()
            .enumerate()
            .any(|(i, a)| self.client_addresses[..i].contains(a))
            || self
                .service_cidrs
                .iter()
                .enumerate()
                .any(|(i, c)| self.service_cidrs[..i].contains(c))
        {
            return Err(bad("duplicate client address or service CIDR"));
        }
        if matches!(s.transport, Role::Endpoint { .. }) && self.client_addresses.contains(&inner) {
            return Err(bad("endpoint cannot also be a client address"));
        }

        if (matches!(s.transport, Role::Client { .. }) && self.client_addresses.is_empty())
            || self.service_cidrs.is_empty()
            || !(10000..=2000000000).contains(&self.table)
            || !(100..=30000).contains(&self.rule_priority)
        {
            return Err(bad(
                "network needs clients, services and reserved table/priority",
            ));
        }
        for a in &self.client_addresses {
            if !contains(&self.tunnel_pool, *a) {
                return Err(bad("client outside tunnel pool"));
            }
        }
        let inner = s.inner()?.0;
        if !contains(&self.tunnel_pool, inner) {
            return Err(bad("TUN outside pool"));
        }
        if matches!(s.transport, Role::Client { .. }) && !self.client_addresses.contains(&inner) {
            return Err(bad("client address not authorized"));
        }
        for c in &self.service_cidrs {
            let (_, n) = cidr(c)?;
            if n == 0 && !self.internet_exit {
                return Err(bad("default service route unsupported"));
            }
        }
        if self.internet_exit && self.service_cidrs != ["0.0.0.0/0"] {
            return Err(bad("internet exit requires exactly the IPv4 default CIDR"));
        }
        if let Role::Client { entry, .. } = s.transport {
            if !self.internet_exit
                && self
                    .service_cidrs
                    .iter()
                    .any(|c| matches!(entry.ip(), std::net::IpAddr::V4(ip) if contains(c, ip)))
            {
                return Err(bad("entry must be outside selected service CIDRs"));
            }
        }
        if let Some(d) = &self.dns {
            if !matches!(s.transport, Role::Client { .. })
                || !self.service_cidrs.iter().any(|c| contains(c, d.resolver))
                || d.suffixes.is_empty()
            {
                return Err(bad(
                    "private DNS must be client scoped inside service CIDRs",
                ));
            }
            for suffix in &d.suffixes {
                if suffix.is_empty()
                    || suffix.len() > 253
                    || suffix.split('.').any(|p| {
                        p.is_empty()
                            || p.starts_with('-')
                            || p.ends_with('-')
                            || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    })
                {
                    return Err(bad("invalid private DNS suffix"));
                }
            }
        }
        Ok(())
    }
    fn permits(&self, client: bool, inner: Ipv4Addr, b: &[u8]) -> bool {
        if b.len() < 20 || b[0] >> 4 != 4 {
            return false;
        }
        let source = Ipv4Addr::new(b[12], b[13], b[14], b[15]);
        let dest = Ipv4Addr::new(b[16], b[17], b[18], b[19]);
        if client {
            dest == inner && self.service_cidrs.iter().any(|c| contains(c, source))
        } else {
            contains(&self.tunnel_pool, source)
                && self.service_cidrs.iter().any(|c| contains(c, dest))
        }
    }
    fn routes(&self, s: &Settings) -> Vec<String> {
        if matches!(s.transport, Role::Client { .. }) {
            if self.internet_exit {
                vec![]
            } else {
                self.service_cidrs.clone()
            }
        } else {
            vec![self.tunnel_pool.clone()]
        }
    }
}
fn command(program: &str, args: &[String]) -> io::Result<String> {
    let o = Command::new(program).args(args).output()?;
    if !o.status.success() {
        return Err(bad(&format!(
            "{program}: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}
fn ip(args: Vec<String>) -> io::Result<String> {
    command("ip", &args)
}
macro_rules! args {($($a:expr),* $(,)?)=>{vec![$($a.to_string()),*]}}
fn table_rows(table: u32) -> io::Result<String> {
    let o = Command::new("ip")
        .args(["-j", "route", "show", "table", &table.to_string()])
        .output()?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else if String::from_utf8_lossy(&o.stderr).contains("FIB table does not exist") {
        Ok("[]".into())
    } else {
        Err(bad("cannot inspect routing table"))
    }
}
fn journal(s: &Settings) -> PathBuf {
    {
        let mut path = s.journal.as_os_str().to_os_string();
        path.push(".network.json");
        PathBuf::from(path)
    }
}
#[derive(Serialize, Deserialize)]
struct Owned {
    network: Network,
    interface: String,
    routes: Vec<String>,
}
fn array(value: &serde_json::Value) -> io::Result<&Vec<serde_json::Value>> {
    value.as_array().ok_or_else(|| bad("invalid ip JSON"))
}
fn number(v: &serde_json::Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}
fn rule_matches(r: &serde_json::Value, n: &Network, c: &str) -> bool {
    let (dst, len) = c.split_once('/').unwrap();
    r["priority"].as_u64() == Some(n.rule_priority as u64)
        && number(&r["table"]) == Some(n.table as u64)
        && (r["dst"].as_str() == Some(c)
            || r["dst"].as_str() == Some(dst)
                && r["dstlen"].as_u64().unwrap_or(32) == len.parse::<u64>().unwrap())
}
// A destination-restricted earlier rule cannot bypass this profile when its
// destination is disjoint from every route we own. Unknown/inverted selectors
// remain conservative. This permits independent endpoint return routes.
fn rule_may_overlap(r: &serde_json::Value, routes: &[String]) -> bool {
    if r.get("not").is_some() || r.get("invert").is_some() {
        return true;
    }
    let Some(dst) = r["dst"].as_str() else {
        return true;
    };
    let destination = if dst.contains('/') {
        dst.to_owned()
    } else {
        format!("{dst}/{}", r["dstlen"].as_u64().unwrap_or(32))
    };
    let Ok((address, prefix)) = cidr(&destination) else {
        return true;
    };
    routes.iter().any(|route| {
        let Ok((other, other_prefix)) = cidr(route) else {
            return true;
        };
        let common = mask(prefix.min(other_prefix));
        address & common == other & common
    })
}
fn read_owned(s: &Settings) -> io::Result<Option<Owned>> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(journal(s))
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !f.metadata()?.is_file() || f.metadata()?.len() > 1048576 {
        return Err(bad("invalid network journal"));
    }
    let mut bytes = vec![];
    f.read_to_end(&mut bytes)?;
    let o: Owned = serde_json::from_slice(&bytes)?;
    if s.network.as_ref() != Some(&o.network)
        || s.interface != o.interface
        || o.routes != o.network.routes(s)
    {
        return Err(bad(
            "network journal differs; cleanup with original profile",
        ));
    }
    Ok(Some(o))
}
fn persist(s: &Settings, o: &Owned) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = journal(s);
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    f.write_all(&serde_json::to_vec(o)?)?;
    f.sync_all()?;
    fs::rename(tmp, &path)?;
    fs::File::open(path.parent().unwrap())?.sync_all()
}
pub fn prepare(s: &Settings) -> io::Result<()> {
    let Some(n) = &s.network else { return Ok(()) };
    if n.internet_exit && matches!(s.transport, Role::Client { .. }) {
        return Ok(());
    }
    let rows: serde_json::Value = serde_json::from_str(&table_rows(n.table)?)?;
    let rules: serde_json::Value = serde_json::from_str(&ip(args!["-j", "rule", "show"])?)?;
    let existing = read_owned(s)?;
    // Main-table private/direct routes cannot override these policy rules.
    // Refuse earlier overlapping policy rather than silently route around RTP.
    let routes = n.routes(s);
    if array(&rules)?.iter().any(|r| {
        r["priority"]
            .as_u64()
            .is_some_and(|p| p > 0 && p < u64::from(n.rule_priority))
            && rule_may_overlap(r, &routes)
    }) {
        return Err(bad(
            "earlier policy rules could bypass RTP; reserve a higher-precedence priority",
        ));
    }
    if existing.is_none() {
        if !array(&rows)?.is_empty() {
            return Err(bad("routing table already occupied"));
        }
        if array(&rules)?.iter().any(|r| {
            r["priority"].as_u64() == Some(n.rule_priority as u64)
                || number(&r["table"]) == Some(n.table as u64)
        }) {
            return Err(bad("routing rule namespace occupied"));
        }
        persist(
            s,
            &Owned {
                network: n.clone(),
                interface: s.interface.clone(),
                routes: n.routes(s),
            },
        )?;
    }
    // Restart adopts the durable guard without ever releasing its rules.
    if !array(&rows)?.iter().any(|r| {
        r["type"] == "unreachable"
            && r["dst"] == "default"
            && number(&r["protocol"]) == Some(242)
            && r["metric"] == 32760
    }) {
        ip(args![
            "route",
            "add",
            "unreachable",
            "default",
            "table",
            n.table,
            "metric",
            "32760",
            "proto",
            "242"
        ])?;
    }
    for c in n.routes(s) {
        if !array(&rules)?.iter().any(|r| rule_matches(r, n, &c)) {
            ip(args![
                "rule",
                "add",
                "priority",
                n.rule_priority,
                "to",
                c,
                "lookup",
                n.table
            ])?;
        }
    }
    Ok(())
}
pub fn activate(s: &Settings) -> io::Result<()> {
    let Some(n) = &s.network else { return Ok(()) };
    for c in n.routes(s) {
        ip(args![
            "route",
            "add",
            c,
            "dev",
            s.interface,
            "table",
            n.table,
            "proto",
            "242"
        ])?;
    }
    Ok(())
}
pub fn cleanup(s: &Settings) -> io::Result<()> {
    let Some(o) = read_owned(s)? else {
        return Ok(());
    };
    // Exact owned selectors, never flush a table, replace a route, or revert
    // another interface. Device routes vanish with our nonpersistent TUN.
    for c in &o.routes {
        let existing: serde_json::Value = serde_json::from_str(&ip(args!["-j", "rule", "show"])?)?;
        if array(&existing)?
            .iter()
            .any(|r| rule_matches(r, &o.network, c))
        {
            ip(args![
                "rule",
                "del",
                "priority",
                o.network.rule_priority,
                "to",
                c,
                "lookup",
                o.network.table
            ])?;
        }
    }
    let rows: serde_json::Value = serde_json::from_str(&table_rows(o.network.table)?)?;
    if array(&rows)?.iter().any(|r| {
        r["type"] == "unreachable"
            && r["dst"] == "default"
            && number(&r["protocol"]) == Some(242)
            && r["metric"] == 32760
    }) {
        ip(args![
            "route",
            "del",
            "unreachable",
            "default",
            "table",
            o.network.table,
            "metric",
            "32760",
            "proto",
            "242"
        ])?;
    }
    fs::remove_file(journal(s))?;
    fs::File::open(journal(s).parent().unwrap())?.sync_all()
}
/// Which packets may be written to this host's TUN: with a configured network, only
/// traffic between the tunnel and its service CIDRs. Others are silently discarded.
pub type WritePolicy = Box<dyn Fn(&[u8]) -> bool + Send>;
pub fn write_policy(settings: &Settings) -> io::Result<WritePolicy> {
    let client = matches!(settings.transport, Role::Client { .. });
    let inner = settings.inner()?.0;
    let network = settings.network.clone();
    Ok(Box::new(move |b| {
        network.as_ref().is_none_or(|n| n.permits(client, inner, b))
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn earlier_return_rules_allow_only_proven_disjoint_destinations() {
        use serde_json::json;
        let routes = vec!["10.88.0.2/32".into()];
        assert!(!rule_may_overlap(&json!({"dst":"10.90.0.2"}), &routes));
        assert!(!rule_may_overlap(&json!({"dst":"10.90.0.0/24"}), &routes));
        for rule in [
            json!({}),
            json!({"dst":"all"}),
            json!({"dst":"0.0.0.0/0"}),
            json!({"dst":"10.88.0.0/24"}),
            json!({"dst":"10.88.0.2"}),
            json!({"dst":"10.90.0.2", "not":true}),
        ] {
            assert!(rule_may_overlap(&rule, &routes), "{rule}");
        }
    }
    #[test]
    fn automatic_dns_is_rejected() {
        assert!(serde_json::from_str::<Dns>(
            r#"{"resolver":"10.2.0.53","suffixes":["corp.example"]}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Dns>(
            r#"{"mode":"systemd_resolved","resolver":"10.2.0.53","suffixes":["corp.example"]}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Dns>(r#"{"mode":"externally_managed_fail_closed","resolver":"10.2.0.53","suffixes":["corp.example"]}"#).is_ok());
    }
    #[test]
    fn packet_policy_rejects_spoofing_and_unselected_services() {
        let n = Network {
            internet_exit: false,
            tunnel_pool: "10.88.0.0/24".into(),
            client_addresses: vec!["10.88.0.2".parse().unwrap()],
            service_cidrs: vec!["10.90.0.0/24".into()],
            table: 18800,
            rule_priority: 1000,
            dns: None,
        };
        let inner = "10.88.0.2".parse().unwrap();
        let mut b = [0u8; 20];
        b[0] = 0x45;
        b[12..16].copy_from_slice(&[10, 88, 0, 2]);
        b[16..20].copy_from_slice(&[10, 90, 0, 2]);
        assert!(n.permits(false, inner, &b));
        // Allocation-level address checks precede this subnet/service policy.
        b[15] = 3;
        assert!(n.permits(false, inner, &b));
        b[14] = 99;
        assert!(!n.permits(false, inner, &b));
        b[14] = 0;
        b[15] = 2;
        b[16..20].copy_from_slice(&[1, 1, 1, 1]);
        assert!(!n.permits(false, inner, &b));
        b[12..16].copy_from_slice(&[10, 90, 0, 2]);
        b[16..20].copy_from_slice(&[10, 88, 0, 2]);
        assert!(n.permits(true, inner, &b));
        b[19] = 3;
        assert!(!n.permits(true, inner, &b));
        assert!(!n.permits(false, inner, &b[..19]));
    }
    #[test]
    fn canonical_cidrs() {
        assert!(cidr("10.2.0.1/24").is_err());
        assert!(contains("10.2.0.0/24", "10.2.0.53".parse().unwrap()));
    }
}
