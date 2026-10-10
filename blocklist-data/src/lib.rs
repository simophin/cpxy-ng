//! Domain blocklists for ad blocking, shared by everything that sees a hostname: DNS queries now,
//! TLS SNI later.
//!
//! A blocked name blocks all its subdomains too. The [`BASELINE`] list is compiled into the
//! binary already sorted, so looking a name up is a binary search per label with nothing to load
//! at startup.

use std::borrow::Cow;

mod compile;

/// The list shipped with the app, see `SOURCE.md`.
pub static BASELINE: Blocklist = Blocklist {
    names: Cow::Borrowed(include_bytes!(concat!(env!("OUT_DIR"), "/names.bin"))),
    index: Cow::Borrowed(include_bytes!(concat!(env!("OUT_DIR"), "/index.bin"))),
};

/// A sorted set of blocked names, in the form `compile.rs` produces.
pub struct Blocklist {
    names: Cow<'static, [u8]>,
    index: Cow<'static, [u8]>,
}

impl Blocklist {
    /// Compiles a list at runtime, for lists that are not embedded and for tests.
    pub fn from_list(text: &str) -> Result<Self, String> {
        let compiled = compile::compile(text)?;
        Ok(Self {
            names: Cow::Owned(compiled.names),
            index: Cow::Owned(compiled.index),
        })
    }

    /// How many names are blocked, not counting the subdomains of other blocked names.
    pub fn len(&self) -> usize {
        (self.index.len() / 4).saturating_sub(1)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether `name` or one of its parent domains is blocked. Takes names as they appear in DNS
    /// (`ads.example.com.`) or in SNI (`ads.example.com`), in any case.
    pub fn is_blocked(&self, name: &str) -> bool {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        std::iter::once(name.as_str())
            .chain(name.match_indices('.').map(|(i, _)| &name[i + 1..]))
            .any(|suffix| self.contains(suffix.as_bytes()))
    }

    fn contains(&self, name: &[u8]) -> bool {
        let (mut low, mut high) = (0, self.len());
        while low < high {
            let mid = low + (high - low) / 2;
            match self.name(mid).cmp(name) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return true,
            }
        }
        false
    }

    fn name(&self, i: usize) -> &[u8] {
        &self.names[self.offset(i)..self.offset(i + 1)]
    }

    fn offset(&self, i: usize) -> usize {
        let bytes = &self.index[i * 4..i * 4 + 4];
        u32::from_le_bytes(bytes.try_into().unwrap()) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compile::parse_line;

    #[test]
    fn parses_list_formats() {
        let cases = [
            ("ads.example.com", Some("ads.example.com")),
            ("0.0.0.0 ads.example.com", Some("ads.example.com")),
            (
                "127.0.0.1\tads.example.com # tracker",
                Some("ads.example.com"),
            ),
            ("||ads.example.com^", Some("ads.example.com")),
            ("||ads.example.com^$important", Some("ads.example.com")),
            ("# comment", None),
            ("! adblock comment", None),
            ("[Adblock Plus 2.0]", None),
            ("127.0.0.1 localhost", None),
            ("0.0.0.0 0.0.0.0", None),
            ("192.168.1.1 router.lan", None),
            ("||ads.example.com^$client=1.2.3.4", None),
            ("||ads.example.com/banner", None),
            ("example.com##.banner", None),
            ("/ads[0-9]+/", None),
            ("||*.example.com^", None),
        ];
        for (line, expected) in cases {
            assert_eq!(parse_line(line), expected, "{line:?}");
        }
    }

    #[test]
    fn blocks_names_and_their_subdomains() {
        let list = Blocklist::from_list(
            "! a list\n\
             ||Ads.Example.com^\n\
             ||x.ads.example.com^\n\
             0.0.0.0 tracker.net\n\
             garbage line here\n",
        )
        .unwrap();
        // x.ads.example.com is covered by its parent
        assert_eq!(list.len(), 2);

        assert!(list.is_blocked("ads.example.com."));
        assert!(list.is_blocked("x.ADS.example.com"));
        assert!(list.is_blocked("tracker.net"));
        assert!(list.is_blocked("a.b.tracker.net."));
        assert!(!list.is_blocked("example.com"));
        assert!(!list.is_blocked("badads.example.com"));
        assert!(!list.is_blocked("nottracker.net"));
        assert!(!list.is_blocked("net"));
        assert!(!list.is_blocked(""));
    }

    #[test]
    fn refuses_exception_rules() {
        assert!(Blocklist::from_list("||ads.example.com^\n@@||ok.ads.example.com^").is_err());
    }

    #[test]
    fn empty_list_blocks_nothing() {
        let list = Blocklist::from_list("").unwrap();
        assert!(list.is_empty());
        assert!(!list.is_blocked("example.com"));
    }

    #[test]
    fn baseline_is_sorted_and_finds_every_name() {
        assert!(BASELINE.len() > 10_000);
        for i in 0..BASELINE.len() {
            if i > 0 {
                assert!(BASELINE.name(i - 1) < BASELINE.name(i));
            }
            let name = std::str::from_utf8(BASELINE.name(i)).unwrap();
            assert!(BASELINE.is_blocked(name), "{name}");
        }
    }
}
