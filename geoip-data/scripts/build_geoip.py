#!/usr/bin/env python3
"""Converts a DB-IP "IP to Country Lite" CSV into the vendored `data/geoip.dat`.

Usage: build_geoip.py <dbip-country-lite-YYYY-MM.csv[.gz]> <output.dat>

The output is the serialized `cpxy_ng::geoip::GeoIPv4Entry` stream: for each IPv4 range,
sorted by start address, 4 bytes start, 4 bytes inclusive end and 2 ASCII bytes country code.
IPv6 rows and DB-IP's unknown country `ZZ` are dropped, and adjacent ranges of the same
country are merged.
"""

import csv
import gzip
import ipaddress
import struct
import sys


def read_ranges(path):
    opener = gzip.open if path.endswith(".gz") else open
    with opener(path, "rt", newline="") as f:
        for start, end, country in csv.reader(f):
            if ":" in start or country == "ZZ":
                continue
            if len(country) != 2 or not country.isascii() or not country.isupper():
                raise ValueError(f"Unexpected country code {country!r}")
            yield int(ipaddress.IPv4Address(start)), int(ipaddress.IPv4Address(end)), country


def merge(ranges):
    merged = []
    for start, end, country in sorted(ranges):
        if end < start:
            raise ValueError(f"Invalid range {start}-{end}")
        if merged:
            prev_start, prev_end, prev_country = merged[-1]
            if start <= prev_end:
                raise ValueError(f"Overlapping ranges at {ipaddress.IPv4Address(start)}")
            if start == prev_end + 1 and country == prev_country:
                merged[-1] = (prev_start, end, country)
                continue
        merged.append((start, end, country))
    return merged


def main():
    source, output = sys.argv[1:]
    entries = merge(read_ranges(source))
    with open(output, "wb") as f:
        for start, end, country in entries:
            f.write(struct.pack(">II2s", start, end, country.encode("ascii")))
    print(f"Wrote {len(entries)} ranges to {output}")


if __name__ == "__main__":
    main()
