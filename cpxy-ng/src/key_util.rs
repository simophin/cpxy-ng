pub fn derive_password(password: &str) -> [u8; 32] {
    // Use sha256 to derive password
    let hash = sha256::digest(password);
    let mut key = [0u8; 32];
    key.copy_from_slice(hash.as_bytes());
    key
}
