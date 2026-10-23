//! Settings, from KEY=value pairs: the environment (systemd's EnvironmentFile), over an optional
//! config file. SERVER holds the key, so it is never a flag.

use anyhow::{Context, bail};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

const DEFAULT_MTU: u16 = 1280;
pub const DEFAULT_HOSTNAME: &str = "cpxy-tsgw";
/// One server each: every query goes to every server. AliDNS for the local region.
const DEFAULT_DNS_UPSTREAM: &str = "223.5.5.5";
/// Asked through the cpxy server, so neither poisoned nor blocked.
const DEFAULT_DNS_ALTERNATIVE: &str = "tcp://1.1.1.1?via=cpxy";
/// Proxied IPv6 needs a cpxy server that reaches IPv6; "direct" relays only what goes direct.
const DEFAULT_IPV6: &str = "full";

/// The engine's JSON config (mobile-engine/src/config.rs).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EngineConfig {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proxy_routes: Vec<String>,
    /// The cpxy server URL with its key.
    pub server: String,
    pub dns_upstream: Vec<String>,
    pub dns_alternative: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dns_search_domains: Vec<String>,
    pub mtu: u16,
    /// Which IPv6 to relay: off, direct or full.
    pub ipv6: String,
    /// The TCP window towards clients, in bytes; the engine's default when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp_window: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub engine: EngineConfig,
    pub hostname: String,
    pub state_dir: String,
    pub control_url: String,
    pub auth_key: Option<String>,
    /// The UDP port WireGuard listens on; 0 lets Tailscale pick one.
    pub port: u16,
}

/// Parses KEY=value lines; blank lines and # comments are skipped, and one level of matching
/// quotes around a value is removed, as systemd's EnvironmentFile does.
pub fn read_config_file(path: &Path) -> anyhow::Result<HashMap<String, String>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse_config(&text)
        .map_err(|line| anyhow::anyhow!("{}:{line}: expected KEY=value", path.display()))
}

/// The pairs in `text`, or the number of the first line that is not one.
fn parse_config(text: &str) -> Result<HashMap<String, String>, usize> {
    let mut values = HashMap::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or(i + 1)?;
        let mut value = value.trim();
        let bytes = value.as_bytes();
        if bytes.len() >= 2
            && (bytes[0] == b'"' || bytes[0] == b'\'')
            && bytes[bytes.len() - 1] == bytes[0]
        {
            value = &value[1..value.len() - 1];
        }
        values.insert(key.trim().to_owned(), value.to_owned());
    }
    Ok(values)
}

/// Reads the settings, each from `lookup` or else `file`.
pub fn load(
    file: &HashMap<String, String>,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<Settings> {
    let get = |key: &str, fallback: &str| -> String {
        lookup(key)
            .filter(|v| !v.is_empty())
            .or_else(|| file.get(key).filter(|v| !v.is_empty()).cloned())
            .unwrap_or_else(|| fallback.to_owned())
    };

    let server = get("SERVER", "");
    if server.is_empty() {
        bail!("SERVER is required: the cpxy server URL with its key");
    }
    let proxy_routes = split_list(&get("PROXY_ROUTES", ""));
    for route in &proxy_routes {
        if route.parse::<ipnet::IpNet>().is_err() {
            bail!("invalid proxy route CIDR: {route}");
        }
    }
    let ipv6 = get("IPV6", DEFAULT_IPV6);
    if !matches!(ipv6.as_str(), "off" | "direct" | "full") {
        bail!("invalid IPV6 {ipv6:?}: expected off, direct or full");
    }
    let mtu = get("MTU", &DEFAULT_MTU.to_string())
        .parse()
        .context("invalid MTU")?;
    let port = get("TS_PORT", "0").parse().context("invalid TS_PORT")?;
    let tcp_window = match get("TCP_WINDOW", "").as_str() {
        "" => None,
        v => Some(v.parse().context("invalid TCP_WINDOW")?),
    };
    let auth_key = Some(get("TS_AUTHKEY", "")).filter(|k| !k.is_empty());

    Ok(Settings {
        engine: EngineConfig {
            proxy_routes,
            server,
            dns_upstream: split_list(&get("DNS_UPSTREAM", DEFAULT_DNS_UPSTREAM)),
            dns_alternative: split_list(&get("DNS_ALTERNATIVE", DEFAULT_DNS_ALTERNATIVE)),
            dns_search_domains: split_list(&get("DNS_SEARCH_DOMAINS", "")),
            mtu,
            ipv6,
            tcp_window,
        },
        // Not HOSTNAME, which shells and containers set to the machine's name.
        hostname: get("TS_HOSTNAME", DEFAULT_HOSTNAME),
        // systemd's StateDirectory= sets STATE_DIRECTORY.
        state_dir: get("STATE_DIR", &get("STATE_DIRECTORY", "")),
        control_url: get("CONTROL_URL", ""),
        auth_key,
        port,
    })
}

/// Splits on commas and spaces, as cpxy-router's settings do.
fn split_list(s: &str) -> Vec<String> {
    s.split([',', ' ', '\t'])
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn defaults() {
        let s = load(&HashMap::new(), env(&[("SERVER", "https://:k@h:1")])).unwrap();
        assert_eq!(s.hostname, DEFAULT_HOSTNAME);
        assert_eq!(s.engine.mtu, 1280);
        assert_eq!(s.engine.ipv6, "full");
        assert_eq!(s.engine.dns_upstream, ["223.5.5.5"]);
        assert_eq!(s.engine.dns_alternative, ["tcp://1.1.1.1?via=cpxy"]);
        assert_eq!(s.port, 0);
        assert_eq!(s.auth_key, None);
        assert_eq!(s.state_dir, "");
    }

    #[test]
    fn environment_wins_over_the_file() {
        let file = parse_config(
            "SERVER='https://:file@h:1'\n# comment\n\nTS_HOSTNAME = \"from-file\"\nTS_PORT=41641\n",
        )
        .unwrap();
        let s = load(&file, env(&[("TS_HOSTNAME", "from-env"), ("TS_PORT", "")])).unwrap();
        assert_eq!(s.engine.server, "https://:file@h:1");
        assert_eq!(s.hostname, "from-env");
        // An empty variable falls back to the file.
        assert_eq!(s.port, 41641);
    }

    #[test]
    fn state_directory_from_systemd() {
        let s = load(
            &HashMap::new(),
            env(&[("SERVER", "x"), ("STATE_DIRECTORY", "/var/lib/x")]),
        )
        .unwrap();
        assert_eq!(s.state_dir, "/var/lib/x");
        let s = load(
            &HashMap::new(),
            env(&[
                ("SERVER", "x"),
                ("STATE_DIRECTORY", "/a"),
                ("STATE_DIR", "/b"),
            ]),
        )
        .unwrap();
        assert_eq!(s.state_dir, "/b");
    }

    #[test]
    fn lists_split_on_commas_and_spaces() {
        let s = load(
            &HashMap::new(),
            env(&[
                ("SERVER", "x"),
                ("PROXY_ROUTES", "10.0.0.0/8, 192.168.0.0/16"),
                ("DNS_SEARCH_DOMAINS", "a b,c"),
            ]),
        )
        .unwrap();
        assert_eq!(s.engine.proxy_routes, ["10.0.0.0/8", "192.168.0.0/16"]);
        assert_eq!(s.engine.dns_search_domains, ["a", "b", "c"]);
    }

    #[test]
    fn rejects_bad_values() {
        assert!(load(&HashMap::new(), env(&[])).is_err());
        for (k, v) in [
            ("PROXY_ROUTES", "nope"),
            ("IPV6", "maybe"),
            ("MTU", "big"),
            ("TS_PORT", "70000"),
            ("TCP_WINDOW", "1M"),
        ] {
            assert!(
                load(&HashMap::new(), env(&[("SERVER", "x"), (k, v)])).is_err(),
                "{k}={v}"
            );
        }
        assert_eq!(parse_config("A=1\nnot a pair\n"), Err(2));
    }

    #[test]
    fn engine_json_omits_empty_lists() {
        let s = load(&HashMap::new(), env(&[("SERVER", "x")])).unwrap();
        let json = serde_json::to_value(&s.engine).unwrap();
        assert!(json.get("proxy_routes").is_none());
        assert!(json.get("tcp_window").is_none());
        let s = load(
            &HashMap::new(),
            env(&[("SERVER", "x"), ("TCP_WINDOW", "524288")]),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&s.engine).unwrap()["tcp_window"],
            524288
        );
        assert_eq!(json["mtu"], 1280);
        assert_eq!(json["ipv6"], "full");
    }
}
