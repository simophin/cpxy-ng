//! Exact IPv4 kernel-route coverage for the same regional/private policy as the engine.
//! u64 exclusive ends let the compiler represent the entire IPv4 address space safely.

/// Regional/private prefixes minus forced proxy ranges (inclusive IPv4 start/end).
/// Returns network addresses and prefix lengths in ascending order, without overlaps.
pub fn direct_ipv4(proxy: &[(u32, u32)]) -> Vec<(u32, u8)> {
    let mut ranges: Vec<_> = crate::CN
        .chunks_exact(8)
        .map(|r| {
            (
                u32::from_be_bytes(r[..4].try_into().unwrap()) as u64,
                u32::from_be_bytes(r[4..].try_into().unwrap()) as u64 + 1,
            )
        })
        .collect();
    // Keep in sync with client::outbound::is_private. Tests compare the effective policies.
    for (start, end) in [
        (0x0a000000, 0x0b000000),
        (0x7f000000, 0x80000000),
        (0xa9fe0000, 0xa9ff0000),
        (0xac100000, 0xac200000),
        (0xc0a80000, 0xc0a90000),
    ] {
        ranges.push((start, end));
    }
    let excluded = proxy
        .iter()
        .map(|&(start, end)| (start as u64, end as u64 + 1))
        .collect();
    compile(ranges, excluded)
}

/// The complement of [`direct_ipv4`], for platforms that route everything else into a tunnel
/// with a list of included routes. Direct prefixes longer than `longest_direct` are left in
/// the tunnel, which keeps the list short: such traffic still works, it just is not direct.
pub fn tunnel_ipv4(proxy: &[(u32, u32)], longest_direct: u8) -> Vec<(u32, u8)> {
    let direct = direct_ipv4(proxy)
        .into_iter()
        .filter(|&(_, prefix)| prefix <= longest_direct)
        .map(|(ip, prefix)| (ip as u64, ip as u64 + (1u64 << (32 - prefix))))
        .collect();
    compile(vec![(0, 1 << 32)], direct)
}

fn merge(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ranges {
        assert!(start < end && end <= 1 << 32);
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

fn compile(ranges: Vec<(u64, u64)>, excluded: Vec<(u64, u64)>) -> Vec<(u32, u8)> {
    let excluded = merge(excluded);
    let mut result = Vec::new();
    for (mut start, end) in merge(ranges) {
        for &(cut_start, cut_end) in &excluded {
            if cut_end <= start {
                continue;
            }
            if cut_start >= end {
                break;
            }
            emit(start, cut_start.min(end), &mut result);
            start = start.max(cut_end);
            if start >= end {
                break;
            }
        }
        emit(start, end, &mut result);
    }
    result
}

fn emit(mut start: u64, end: u64, result: &mut Vec<(u32, u8)>) {
    while start < end {
        let bits = start.trailing_zeros().min(32).min((end - start).ilog2());
        result.push((start as u32, (32 - bits) as u8));
        start += 1u64 << bits;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_space_and_endpoints() {
        assert_eq!(compile(vec![(0, 1 << 32)], vec![]), vec![(0, 0)]);
        assert_eq!(
            compile(vec![(u32::MAX as u64, 1 << 32)], vec![]),
            vec![(u32::MAX, 32)]
        );
        assert!(compile(vec![(0, 1 << 32)], vec![(0, 1 << 32)]).is_empty());
    }

    #[test]
    fn exact_coverage_after_overlapping_exclusions() {
        let included = vec![(0, 37), (30, 64), (129, 255)];
        let cuts = vec![(0, 1), (7, 15), (12, 20), (60, 140), (254, 256)];
        let prefixes = compile(included.clone(), cuts.clone());
        for ip in 0..256u64 {
            let expected = included.iter().any(|&(a, b)| a <= ip && ip < b)
                && !cuts.iter().any(|&(a, b)| a <= ip && ip < b);
            let matches = prefixes
                .iter()
                .filter(|&&(a, p)| a as u64 <= ip && ip < a as u64 + (1u64 << (32 - p)))
                .count();
            assert_eq!(matches, usize::from(expected), "{ip}");
        }
    }

    #[test]
    fn tunnel_is_the_exact_complement_of_direct() {
        let forced = [(0x0ae90002, 0x0ae90002)];
        let direct = direct_ipv4(&forced);
        let tunnel = tunnel_ipv4(&forced, 32);
        let size = |prefixes: &[(u32, u8)]| -> u64 {
            prefixes.iter().map(|&(_, p)| 1u64 << (32 - p)).sum()
        };
        assert_eq!(size(&direct) + size(&tunnel), 1 << 32);
        let covers = |prefixes: &[(u32, u8)], ip: u32| {
            prefixes.iter().any(|&(a, p)| {
                (ip as u64) >= a as u64 && (ip as u64) < a as u64 + (1u64 << (32 - p))
            })
        };
        for ip in [0x0ae90002, 0x08080808, 0x0a000001, 0xe0000001] {
            assert!(covers(&direct, ip) != covers(&tunnel, ip), "{ip:x}");
        }
        assert!(covers(&tunnel, 0x0ae90002));
        assert!(covers(&direct, 0x0a000001));

        let coarse = tunnel_ipv4(&forced, 18);
        assert!(coarse.len() < tunnel.len() / 4);
        // Coarsening only moves direct addresses into the tunnel, never the reverse.
        for &(ip, _) in &tunnel {
            assert!(covers(&coarse, ip), "{ip:x}");
        }
        assert!(!covers(&coarse, 0x0a000001));
    }

    #[test]
    fn broad_proxy_exclusion_wins_over_regional_specifics() {
        assert!(direct_ipv4(&[(0, u32::MAX)]).is_empty());
        let prefixes = direct_ipv4(&[(0x72000000, 0x72ffffff)]);
        assert!(!prefixes.iter().any(|&(ip, _)| ip >> 24 == 114));
    }
}
