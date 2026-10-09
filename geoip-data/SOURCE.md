# Vendored GeoIP data

`data/geoip.dat` is the reviewed, build-time input embedded by the `geoip-data` crate. It maps
IPv4 ranges to ISO 3166-1 alpha-2 country codes for every country; China routing looks for `CN`.

- Upstream: [DB-IP IP to Country Lite](https://db-ip.com/db/download/ip-to-country-lite), `dbip-country-lite-2026-10.csv.gz`
- License: [Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/). "IP Geolocation by DB-IP" (https://db-ip.com) must be credited wherever the data is shown.
- Retrieval date: 2026-10-09
- Upstream file SHA-256: `097426b8ddae89157d444a59ac1847e873f7943c32d52becc4371c8b0273af80`
- Vendored format: the project's compact serialized `GeoIPv4Entry` stream
- Transformation (`scripts/build_geoip.py`): drop IPv6 rows and the unknown country `ZZ`, merge adjacent ranges of the same country
- Ranges: 362,108
- File size: 3,621,080 bytes
- SHA-256: `150dba0a921e014d6284b4fdb14803aca9e585e6cad41022e9be0f07ee454683`

Normal Cargo and Gradle builds never fetch GeoIP data from the network.

## Updating the data

1. Download the desired monthly `dbip-country-lite-YYYY-MM.csv.gz` from DB-IP.
2. Run `python3 scripts/build_geoip.py <downloaded.csv.gz> data/geoip.dat`.
3. Update the upstream file name, date, checksums, range count and size in this file, and `EXPECTED_SHA256` in `build.rs`, in the same commit.
4. Run the locked offline native build (`cargo build --locked --offline`) and `cargo test --workspace`.
