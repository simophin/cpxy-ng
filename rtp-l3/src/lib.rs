//! Authenticated HTTP-controlled RTP-shaped IPv4 transport v2; not SRTP.
pub mod v2;
pub const PROTOCOL: &str = "rtp-http-v2";
pub const MTU: usize = 1200;
fn ipv4(p: &[u8]) -> bool {
    p.len() >= 20
        && p.len() <= MTU
        && p[0] >> 4 == 4
        && (p[0] & 15) >= 5
        && (p[0] as usize & 15) * 4 <= p.len()
        && u16::from_be_bytes([p[2], p[3]]) as usize == p.len()
}

/// Both sides derive the shared key independently, so the salt is a fixed,
/// versioned protocol constant. Changing it or the parameters breaks interop.
const PSK_SALT: &[u8] = b"cpxy-rtp-l3/psk/v1";
pub const MIN_PASSWORD_LEN: usize = 12;
/// Shared 32-byte key from a password: Argon2id, 19 MiB, 2 passes, 1 lane.
pub fn derive_psk(password: &str) -> Result<[u8; 32], &'static str> {
    use argon2::{Algorithm, Argon2, Params, Version};
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err("password must be at least 12 characters");
    }
    let params = Params::new(19 * 1024, 2, 1, Some(32)).map_err(|_| "invalid KDF parameters")?;
    let mut psk = [0; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), PSK_SALT, &mut psk)
        .map_err(|_| "key derivation failed")?;
    Ok(psk)
}

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(unix)]
pub mod worker;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn password_derivation_remains_compatible() {
        let key = derive_psk("correct horse battery").unwrap();
        assert_eq!(&key[..4], &[96, 103, 105, 76]);
        assert_eq!(key, derive_psk("correct horse battery").unwrap());
        assert!(derive_psk("short").is_err());
    }
}
