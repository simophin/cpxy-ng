# Vendored GeoIP data

Two files are built from the same DB-IP snapshot, so the traffic log's flags agree with routing:

- `data/cn.dat`, embedded by the `geoip-data` crate: the IPv4 ranges of the local region (`CN`),
  for `cpxy_ng::geoip::contains_v4`. Routing and the DNS split only ask whether an address is in
  the local region.
- `mobile/androidApp/src/main/assets/country.dat`, bundled with the Android app: the IPv4 ranges
  of every country, with their ISO 3166-1 alpha-2 codes, for the traffic log's flags
  (`CountryLookup`). Keeping it out of the engine library keeps the router packages small.

- Upstream: [DB-IP IP to Country Lite](https://db-ip.com/db/download/ip-to-country-lite), `dbip-country-lite-2026-10.csv.gz`
- License: [Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/). "IP Geolocation by DB-IP" (https://db-ip.com) must be credited wherever the data is shown.
- Retrieval date: 2026-10-09
- Upstream file SHA-256: `097426b8ddae89157d444a59ac1847e873f7943c32d52becc4371c8b0273af80`
- Transformation (`scripts/build_geoip.py`): drop IPv6 rows and the unknown country `ZZ`, merge adjacent ranges of the same country
- Format: ranges sorted by start, as big-endian 4 bytes start and 4 bytes inclusive end; `country.dat` follows each with its 2 ASCII bytes country code
- `cn.dat`: 5,424 ranges, 43,392 bytes, SHA-256 `e7059375cb244a9391df14b90782c34c10325321275d48cfe8e357649da17140`
- `country.dat`: 362,108 ranges, 3,621,080 bytes, SHA-256 `150dba0a921e014d6284b4fdb14803aca9e585e6cad41022e9be0f07ee454683`

Normal Cargo and Gradle builds never fetch GeoIP data from the network.

## Updating the data

1. Download the desired monthly `dbip-country-lite-YYYY-MM.csv.gz` from DB-IP.
2. From the repository root, run `python3 geoip-data/scripts/build_geoip.py <downloaded.csv.gz> geoip-data/data/cn.dat mobile/androidApp/src/main/assets/country.dat`.
3. Update the upstream file name, date, checksums, range counts and sizes in this file, and `EXPECTED_SHA256` in `build.rs`, in the same commit.
4. Run the locked offline native build (`cargo build --locked --offline`), `cargo test --workspace` and `./gradlew :shared:allTests` in `mobile`.
