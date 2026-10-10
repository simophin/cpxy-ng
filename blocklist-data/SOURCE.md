# Ad blocklist

The `blocklist-data` crate embeds an ad and tracker blocklist, used by the mobile VPN engine and
OpenWrt's `dns_split` when ad blocking is on.

- Upstream: [HaGeZi's Multi NORMAL](https://github.com/hagezi/dns-blocklists), adblock format,
  `https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/multi.txt`
- License: [GPL-3.0](https://github.com/hagezi/dns-blocklists/blob/main/LICENSE)
- Embedded format (`build.rs`, `src/compile.rs`): the names sorted and concatenated, plus an index
  of `u32` offsets, about 3.8 MB; lookups are a binary search per label of the queried name

The list is not committed. `build.rs` downloads it into `data/hagezi-multi.txt` (gitignored) when
that file is missing, and uses the downloaded copy from then on. Local builds therefore download it
once, and CI downloads the latest list on every run because each checkout starts without it. The
first build needs network access.

`build.rs` refuses a list that is truncated (fewer than 10,000 names) or has `@@` exception rules,
and only caches a list it accepted.

## Refreshing the local copy

Delete `data/hagezi-multi.txt`; the next build downloads the list again.
