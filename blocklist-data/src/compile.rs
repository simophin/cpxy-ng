//! Turns a blocklist into the sorted form [`crate::Blocklist`] searches. `build.rs` includes this
//! file too, so it uses only `std`.
//!
//! Understands the formats public lists are published in:
//! - the DNS subset of adblock syntax: `||ads.example.com^`
//! - hosts files: `0.0.0.0 ads.example.com`
//! - plain domain lists: `ads.example.com`
//!
//! Rules that cannot be decided from a hostname alone (cosmetic rules, paths, wildcards, regexes,
//! `$` modifiers) are skipped. Exception rules (`@@`) are refused rather than skipped: ignoring
//! one would block what the list means to allow.

use std::collections::BTreeSet;

pub struct Compiled {
    /// Every blocked name, sorted, lowercase, without separators.
    pub names: Vec<u8>,
    /// Where each name starts in `names`, as little-endian `u32`s, plus the end of the last one.
    pub index: Vec<u8>,
}

pub fn compile(text: &str) -> Result<Compiled, String> {
    let mut names = BTreeSet::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.starts_with("@@") {
            return Err(format!(
                "line {}: exception rules are not supported: {line}",
                n + 1
            ));
        }
        if let Some(name) = parse_line(line) {
            names.insert(name.to_ascii_lowercase());
        }
    }

    // A name under another blocked name is blocked through its parent already.
    let names: Vec<String> = names
        .iter()
        .filter(|name| {
            !name
                .match_indices('.')
                .any(|(i, _)| names.contains(&name[i + 1..]))
        })
        .cloned()
        .collect();

    let mut compiled = Compiled {
        names: Vec::new(),
        index: Vec::with_capacity((names.len() + 1) * 4),
    };
    for name in &names {
        compiled
            .index
            .extend_from_slice(&offset(compiled.names.len())?);
        compiled.names.extend_from_slice(name.as_bytes());
    }
    compiled
        .index
        .extend_from_slice(&offset(compiled.names.len())?);
    Ok(compiled)
}

fn offset(n: usize) -> Result<[u8; 4], String> {
    u32::try_from(n)
        .map(u32::to_le_bytes)
        .map_err(|_| "blocklist too large".to_string())
}

pub fn parse_line(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(['#', '!', '[']) {
        return None;
    }

    if let Some(rule) = line.strip_prefix("||") {
        // `$important` only matters between overlapping browser rules. Other modifiers narrow
        // the rule to some clients or record types.
        let rule = rule.strip_suffix("$important").unwrap_or(rule);
        return valid_domain(rule.strip_suffix('^')?);
    }

    // Hosts file or plain domain list, possibly with a trailing comment. A `#` right after the
    // domain is an adblock cosmetic rule (`example.com##.banner`), not a comment.
    let line = match line.find('#') {
        Some(i) if line[..i].ends_with(char::is_whitespace) => &line[..i],
        Some(_) => return None,
        None => line,
    };
    let mut fields = line.split_whitespace();
    let first = fields.next()?;
    let domain = match fields.next() {
        // Only sinkhole addresses: a hosts file that maps a name to a real address is not a
        // blocklist entry.
        Some(domain) if matches!(first, "0.0.0.0" | "127.0.0.1" | "::" | "::1") => domain,
        Some(_) => return None,
        None => first,
    };
    valid_domain(domain)
}

fn valid_domain(domain: &str) -> Option<&str> {
    let domain = domain.trim_end_matches('.');
    let valid = domain.contains('.')
        && domain
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
        && !domain.starts_with('.')
        && !domain.contains("..")
        // Hosts files map their own addresses too.
        && domain.parse::<std::net::IpAddr>().is_err();
    valid.then_some(domain)
}
