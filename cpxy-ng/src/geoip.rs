use anyhow::{Context, ensure};
use std::cmp::Ordering;
use std::io::Write;
use std::net::Ipv4Addr;

#[repr(C)]
#[derive(Debug, Clone, Eq, PartialEq, Ord)]
pub struct GeoIPv4Entry {
    from: [u8; 4],
    to: [u8; 4],
    country_code: [u8; 2],
}

impl GeoIPv4Entry {
    pub fn from(&self) -> Ipv4Addr {
        self.from.into()
    }

    pub fn to(&self) -> Ipv4Addr {
        self.to.into()
    }

    pub fn new(from: Ipv4Addr, to: Ipv4Addr, country_code: [u8; 2]) -> Self {
        assert!(to >= from, "Invalid IP range");

        Self {
            from: from.octets(),
            to: to.octets(),
            country_code,
        }
    }
}

impl PartialOrd for GeoIPv4Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(
            self.from()
                .cmp(&other.from())
                .then_with(|| self.to().cmp(&other.to())),
        )
    }
}

pub fn find_country_code_v4<'a>(
    ip: &Ipv4Addr,
    sorted_serialized_data: &'a [u8],
) -> anyhow::Result<Option<&'a str>> {
    let entry_size = size_of::<GeoIPv4Entry>();
    ensure!(
        sorted_serialized_data.len() % entry_size == 0,
        "Invalid serialized data length, must be multiple of {entry_size}"
    );

    let entries = unsafe {
        std::slice::from_raw_parts(
            sorted_serialized_data.as_ptr() as *const GeoIPv4Entry,
            sorted_serialized_data.len() / entry_size,
        )
    };

    let code = match entries.binary_search_by(|entry| entry.from().cmp(ip)) {
        Ok(index) => &entries[index].country_code,
        Err(index) => {
            if index == 0 {
                return Ok(None);
            }

            let entry = &entries[index - 1];
            if ip <= &entry.to() {
                &entry.country_code
            } else {
                return Ok(None);
            }
        }
    };

    std::str::from_utf8(code.as_ref())
        .context("Invalid country code")
        .map(Some)
}

pub fn serialize_entries(writer: impl Write, mut entries: Vec<GeoIPv4Entry>) -> anyhow::Result<()> {
    entries.sort();

    let mut writer = writer;
    for entry in entries {
        let bytes: [u8; 10] = unsafe { std::mem::transmute(entry) };
        writer.write_all(&bytes)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_and_mapping_works() {
        let entries = vec![
            GeoIPv4Entry::new(100.into(), 200.into(), *b"US"),
            GeoIPv4Entry::new(0.into(), 50.into(), *b"CN"),
            GeoIPv4Entry::new(50.into(), 100.into(), *b"NZ"),
        ];

        let mut serialized = vec![0u8; 0];
        serialize_entries(&mut serialized, entries).expect("Failed to serialize entries");

        assert_eq!(
            find_country_code_v4(&25.into(), &serialized).expect("Lookup failed"),
            Some("CN")
        );
        assert_eq!(
            find_country_code_v4(&75.into(), &serialized).expect("Lookup failed"),
            Some("NZ")
        );
    }
}
