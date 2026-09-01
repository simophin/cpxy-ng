# Vendored CN GeoIP data

`data/cn-geoip.dat` is the reviewed, build-time input embedded by the `geoip-data` crate.

- Upstream project: `Loyalsoldier/geoip`
- Upstream URL used by the previous build script: `https://cdn.jsdelivr.net/gh/Loyalsoldier/geoip@release/geoip.dat`
- Retrieval date of the artifact adopted as the reproducibility baseline: 2026-08-28
- Vendored format: the project's compact serialized `GeoIPv4Entry` stream, not the upstream V2Ray protobuf file
- Selection/transformation: retain IPv4 CIDRs whose country code is `CN` or `CHINA`, convert each CIDR to its inclusive start/end range, and assign country code `CN`
- File size: 62,230 bytes
- SHA-256: `76997024829ff7b43948f781c69fd8aa90f4ba1e3d3e3f6b84363fb68a6c8ed1`

The upstream `release` URL is mutable. The checked-in derivative and checksum are therefore the reproducibility boundary: normal Cargo and Gradle builds never fetch GeoIP data from the network.

## Updating the data

1. Fetch the desired upstream revision explicitly and record an immutable upstream commit or release identifier when one is available.
2. Decode its V2Ray `GeoIpList`, select the `CN`/`CHINA` IPv4 CIDRs, and serialize them with `cpxy_ng::geoip::serialize_entries` using the transformation above.
3. Review the source provenance and applicable upstream licensing before redistribution.
4. Replace `data/cn-geoip.dat`, update the date, provenance, size, and SHA-256 in this file, and update `EXPECTED_SHA256` in `build.rs` in the same commit.
5. Run the locked offline native build and Desktop native smoke checks recorded in `docs/kmp-desktop-progress-handoff.md`.
