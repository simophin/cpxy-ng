//! Carries a node's Tailscale identity over from the Go cpxy-tsgw, so an upgrade keeps the same
//! machine on the tailnet, with its approved routes, rather than registering a new one.
//!
//! The Go version (tsnet) kept its state in `tailscaled.state` in the state directory: a JSON
//! object of base64 values, where `_machinekey` is the machine key, `_current-profile` names the
//! profile, and that profile's prefs hold the node key under `Config`. tailscale-rs keeps the
//! same keys in `keys.json`. When the directory has the first and not the second, the keys are
//! written to `keys.json`; the old file stays, in case of a downgrade.

use anyhow::{Context, bail};
use base64::Engine as _;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

pub const KEY_FILE: &str = "keys.json";
const GO_STATE_FILE: &str = "tailscaled.state";

/// Writes `keys.json` from the Go state in `dir` when only the latter exists. Returns whether it
/// did.
pub fn import_go_state(dir: &Path) -> anyhow::Result<bool> {
    let key_file = dir.join(KEY_FILE);
    let go_file = dir.join(GO_STATE_FILE);
    if key_file.exists() || !go_file.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(&go_file)
        .with_context(|| format!("reading {}", go_file.display()))?;
    let keys = keys_from_go_state(&text)
        .with_context(|| format!("reading the keys in {}", go_file.display()))?;
    let json = serde_json::to_string_pretty(&keys)?;
    write_private(&key_file, json.as_bytes())
        .with_context(|| format!("writing {}", key_file.display()))?;
    Ok(true)
}

#[derive(Debug, PartialEq, serde::Serialize)]
struct KeyFile {
    key_state: KeyState,
}

/// `keys.json`'s keys, in the text form both implementations use (`privkey:<hex>`).
#[derive(Debug, PartialEq, serde::Serialize)]
struct KeyState {
    machine_key: String,
    network_lock_key: String,
    node_key: String,
    old_node_key: Option<String>,
    acme_account_key: Option<String>,
}

#[derive(Deserialize)]
struct Prefs {
    #[serde(rename = "Config")]
    config: Option<Persist>,
}

#[derive(Deserialize)]
struct Persist {
    #[serde(rename = "PrivateNodeKey")]
    private_node_key: Option<String>,
    #[serde(rename = "NetworkLockKey")]
    network_lock_key: Option<String>,
}

fn keys_from_go_state(text: &str) -> anyhow::Result<KeyFile> {
    let raw: HashMap<String, String> =
        serde_json::from_str(text).context("not a Tailscale state file")?;
    let value = |key: &str| -> anyhow::Result<Vec<u8>> {
        let encoded = raw.get(key).with_context(|| format!("no {key}"))?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .with_context(|| format!("{key} is not base64"))
    };
    let machine_key =
        String::from_utf8(value("_machinekey")?).context("_machinekey is not text")?;
    let profile =
        String::from_utf8(value("_current-profile")?).context("_current-profile is not text")?;
    let prefs: Prefs = serde_json::from_slice(&value(&profile)?)
        .with_context(|| format!("{profile} is not prefs"))?;
    let persist = prefs
        .config
        .context("the profile has no keys: the node never logged in")?;
    let node_key = persist
        .private_node_key
        .filter(|k| !k.is_empty())
        .context("the profile has no node key")?;
    for (name, key) in [("machine", &machine_key), ("node", &node_key)] {
        if !is_x25519_private(key) {
            bail!("the {name} key is not a private key");
        }
    }
    Ok(KeyFile {
        key_state: KeyState {
            machine_key,
            // Go's Ed25519 key is the seed and the public key; tailscale-rs keeps the seed. A node
            // without one gets a new key, which matters only under Tailnet Lock.
            network_lock_key: match persist
                .network_lock_key
                .as_deref()
                .and_then(network_lock_seed)
            {
                Some(key) => key,
                None => new_network_lock_key()?,
            },
            node_key,
            old_node_key: None,
            acme_account_key: None,
        },
    })
}

fn is_x25519_private(key: &str) -> bool {
    key.strip_prefix("privkey:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn network_lock_seed(key: &str) -> Option<String> {
    let hex = key.strip_prefix("nlpriv:")?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        64 => Some(key.to_owned()),
        128 => Some(format!("nlpriv:{}", &hex[..64])),
        _ => None,
    }
}

/// A random Ed25519 seed: any 32 bytes are one.
fn new_network_lock_key() -> anyhow::Result<String> {
    use std::io::Read;
    let mut seed = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut seed))
        .context("reading /dev/urandom")?;
    Ok(format!(
        "nlpriv:{}",
        seed.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

/// Writes a file only its owner can read, as it holds private keys.
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACHINE: &str =
        "privkey:1111111111111111111111111111111111111111111111111111111111111111";
    const NODE: &str = "privkey:2222222222222222222222222222222222222222222222222222222222222222";
    const NL_SEED: &str = "3333333333333333333333333333333333333333333333333333333333333333";
    const NL_PUB: &str = "4444444444444444444444444444444444444444444444444444444444444444";

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    fn go_state(prefs: &str) -> String {
        serde_json::json!({
            "_machinekey": b64(MACHINE),
            "_current-profile": b64("profile-a1b2"),
            "_profiles": b64("{}"),
            "profile-a1b2": b64(prefs),
        })
        .to_string()
    }

    #[test]
    fn takes_the_machine_node_and_network_lock_keys() {
        let prefs = format!(
            r#"{{"ControlURL":"https://controlplane.tailscale.com","Config":{{"PrivateNodeKey":"{NODE}","OldPrivateNodeKey":"privkey:0000000000000000000000000000000000000000000000000000000000000000","NetworkLockKey":"nlpriv:{NL_SEED}{NL_PUB}","NodeID":"n123"}}}}"#
        );
        let keys = keys_from_go_state(&go_state(&prefs)).unwrap().key_state;
        assert_eq!(keys.machine_key, MACHINE);
        assert_eq!(keys.node_key, NODE);
        assert_eq!(keys.network_lock_key, format!("nlpriv:{NL_SEED}"));
        assert_eq!(keys.old_node_key, None);
    }

    #[test]
    fn writes_keys_json_once_and_keeps_the_go_state() {
        let dir = std::env::temp_dir().join(format!("cpxy-tsgw-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prefs = format!(r#"{{"Config":{{"PrivateNodeKey":"{NODE}"}}}}"#);
        std::fs::write(dir.join(GO_STATE_FILE), go_state(&prefs)).unwrap();
        assert!(import_go_state(&dir).unwrap());
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(KEY_FILE)).unwrap()).unwrap();
        assert_eq!(written["key_state"]["node_key"], NODE);
        assert_eq!(written["key_state"]["machine_key"], MACHINE);
        // No network lock key in the Go state: a new one, in the form tailscale-rs reads.
        let nl = written["key_state"]["network_lock_key"].as_str().unwrap();
        assert!(
            network_lock_seed(nl).is_some_and(|k| k.len() == "nlpriv:".len() + 64),
            "{nl}"
        );
        // Once keys.json exists it is left alone.
        assert!(!import_go_state(&dir).unwrap());
        assert!(dir.join(GO_STATE_FILE).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_node_that_never_logged_in_has_nothing_to_carry() {
        let err = keys_from_go_state(&go_state(r#"{"ControlURL":"x"}"#)).unwrap_err();
        assert!(format!("{err:#}").contains("never logged in"), "{err:#}");
        assert!(keys_from_go_state("{}").is_err());
        assert!(keys_from_go_state("not json").is_err());
    }
}
