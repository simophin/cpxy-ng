use sha2::Digest;

pub fn derive_password(password: &str) -> [u8; 32] {
    sha2::Sha256::digest(password.as_bytes()).into()
}
