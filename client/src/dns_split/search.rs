//! Search suffix validation shared by the DNS binary and packet engine.
use anyhow::{Context, bail};
use hickory_proto::rr::Name;

/// Accept ASCII hostname suffixes, with an optional trailing dot; reject root and IPs.
pub fn parse_search_domain(value: &str) -> anyhow::Result<Name> {
    let domain = value.strip_suffix('.').unwrap_or(value);
    if domain.is_empty()
        || domain.len() > 253
        || (domain.split('.').count() == 4
            && domain.split('.').all(|label| label.parse::<u8>().is_ok()))
        || !domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        bail!("Invalid DNS search domain: {value}");
    }
    let mut name = Name::from_ascii(domain).context("Invalid DNS search domain")?;
    name.set_fqdn(true);
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_suffixes() {
        for value in ["lan", "corp.example.", "my-domain.example"] {
            assert!(parse_search_domain(value).unwrap().is_fqdn());
        }
        for value in [
            "",
            ".",
            "a..b",
            "a..",
            "-lan",
            "lan-",
            "*.example",
            "a_b",
            "https://example",
            "1.2.3.4",
            "a b",
            "é.example",
        ] {
            assert!(parse_search_domain(value).is_err(), "{value}");
        }
        assert!(parse_search_domain(&"a".repeat(64)).is_err());
    }
}
