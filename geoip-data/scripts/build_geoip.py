#!/usr/bin/env python3
"""Converts a DB-IP "IP to Country Lite" CSV into the vendored GeoIP data.

Usage: build_geoip.py <dbip-country-lite-YYYY-MM.csv[.gz]> <cn.dat> <cn6.dat> <country.dat>

All outputs list ranges sorted by start address, as a big-endian start and inclusive end:
- `cn.dat`, for `cpxy_ng::geoip::contains_v4`, has only the IPv4 ranges of the local region
  (`CN`), with 4-byte addresses.
- `cn6.dat`, for `cpxy_ng::geoip::contains_v6`, has only the IPv6 ranges of the local region,
  with 16-byte addresses.
- `country.dat`, for the Android app's traffic log, has the IPv4 ranges of every country, each
  followed by its 2 ASCII bytes country code.

DB-IP's unknown country `ZZ` is dropped, and adjacent ranges of the same country are merged.
"""

import csv
import gzip
import ipaddress
import sys


def read_ranges(path):
    opener = gzip.open if path.endswith(".gz") else open
    with opener(path, "rt", newline="") as f:
        for start, end, country in csv.reader(f):
            if country == "ZZ":
                continue
            if len(country) != 2 or not country.isascii() or not country.isupper():
                raise ValueError(f"Unexpected country code {country!r}")
            start, end = ipaddress.ip_address(start), ipaddress.ip_address(end)
            if start.version != end.version:
                raise ValueError(f"Mixed address families in {start}-{end}")
            yield start.version, int(start), int(end), country


def merge(ranges):
    merged = []
    for start, end, country in sorted(ranges):
        if end < start:
            raise ValueError(f"Invalid range {start}-{end}")
        if merged:
            prev_start, prev_end, prev_country = merged[-1]
            if start <= prev_end:
                raise ValueError(f"Overlapping ranges at {ipaddress.ip_address(start)}")
            if start == prev_end + 1 and country == prev_country:
                merged[-1] = (prev_start, end, country)
                continue
        merged.append((start, end, country))
    return merged


def write_ranges(path, entries, size, country_code=False):
    with open(path, "wb") as f:
        for start, end, country in entries:
            f.write(start.to_bytes(size, "big") + end.to_bytes(size, "big"))
            if country_code:
                f.write(country.encode("ascii"))
    print(f"Wrote {len(entries)} ranges to {path}")


def main():
    source, cn_output, cn6_output, country_output = sys.argv[1:]
    ranges = list(read_ranges(source))
    v4 = merge((start, end, country) for version, start, end, country in ranges if version == 4)
    v6 = merge((start, end, country) for version, start, end, country in ranges if version == 6)

    write_ranges(country_output, v4, 4, country_code=True)
    write_ranges(cn_output, [r for r in v4 if r[2] == "CN"], 4)
    write_ranges(cn6_output, [r for r in v6 if r[2] == "CN"], 16)


if __name__ == "__main__":
    main()
