use std::net::{Ipv4Addr, Ipv6Addr};

/// Whether `ip` is in one of the IPv4 ranges of `sorted_serialized_data`, which holds
/// non-overlapping ranges sorted by start, each as a big-endian 4 bytes start and 4 bytes
/// inclusive end.
pub fn contains_v4(ip: &Ipv4Addr, sorted_serialized_data: &[u8]) -> bool {
    contains::<4, 8>(ip.to_bits().into(), sorted_serialized_data)
}

/// Whether `ip` is in one of the IPv6 ranges of `sorted_serialized_data`, laid out like
/// [`contains_v4`]'s with 16-byte addresses.
pub fn contains_v6(ip: &Ipv6Addr, sorted_serialized_data: &[u8]) -> bool {
    contains::<16, 32>(ip.to_bits(), sorted_serialized_data)
}

/// Each `ENTRY` is a big-endian `ADDR`-byte start and inclusive end.
fn contains<const ADDR: usize, const ENTRY: usize>(
    ip: u128,
    sorted_serialized_data: &[u8],
) -> bool {
    let (entries, rest) = sorted_serialized_data.as_chunks::<ENTRY>();
    debug_assert!(rest.is_empty());
    let read = |bytes: &[u8]| bytes.iter().fold(0u128, |n, b| (n << 8) | u128::from(*b));
    let from = |entry: &[u8; ENTRY]| read(&entry[..ADDR]);
    let to = |entry: &[u8; ENTRY]| read(&entry[ADDR..]);

    // Only the last range starting at or before `ip` can hold it.
    match entries.partition_point(|entry| from(entry) <= ip) {
        0 => false,
        index => ip <= to(&entries[index - 1]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serialize(ranges: &[(u32, u32)]) -> Vec<u8> {
        ranges
            .iter()
            .flat_map(|(from, to)| from.to_be_bytes().into_iter().chain(to.to_be_bytes()))
            .collect()
    }

    fn serialize_v6(ranges: &[(u128, u128)]) -> Vec<u8> {
        ranges
            .iter()
            .flat_map(|(from, to)| from.to_be_bytes().into_iter().chain(to.to_be_bytes()))
            .collect()
    }

    #[test]
    fn finds_addresses_in_ranges() {
        let data = serialize(&[(0, 50), (60, 60), (100, 200), (u32::MAX - 1, u32::MAX)]);

        for ip in [0, 25, 50, 60, 100, 200, u32::MAX - 1, u32::MAX] {
            assert!(contains_v4(&ip.into(), &data), "{ip}");
        }
        for ip in [51, 59, 61, 99, 201, u32::MAX - 2] {
            assert!(!contains_v4(&ip.into(), &data), "{ip}");
        }
        assert!(!contains_v4(&1.into(), &[]));
    }

    #[test]
    fn finds_ipv6_addresses_in_ranges() {
        let data = serialize_v6(&[(0, 50), (1 << 100, (1 << 101) - 1), (u128::MAX, u128::MAX)]);

        for ip in [0, 50, 1 << 100, (1 << 101) - 1, u128::MAX] {
            assert!(contains_v6(&ip.into(), &data), "{ip:x}");
        }
        for ip in [51, (1 << 100) - 1, 1 << 101, u128::MAX - 1] {
            assert!(!contains_v6(&ip.into(), &data), "{ip:x}");
        }
        assert!(!contains_v6(&1.into(), &[]));
    }
}
