use std::net::Ipv4Addr;

/// The size of a range in the serialized data: big-endian start and inclusive end.
const ENTRY_SIZE: usize = 8;

/// Whether `ip` is in one of the IPv4 ranges of `sorted_serialized_data`, which holds
/// non-overlapping ranges sorted by start, each as a big-endian 4 bytes start and 4 bytes
/// inclusive end.
pub fn contains_v4(ip: &Ipv4Addr, sorted_serialized_data: &[u8]) -> bool {
    let (entries, rest) = sorted_serialized_data.as_chunks::<ENTRY_SIZE>();
    debug_assert!(rest.is_empty());
    let ip = u32::from(*ip);
    let from =
        |entry: &[u8; ENTRY_SIZE]| u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]);
    let to =
        |entry: &[u8; ENTRY_SIZE]| u32::from_be_bytes([entry[4], entry[5], entry[6], entry[7]]);

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
}
