use rand::RngCore;
use sha2::Digest;

pub fn derive_password(password: &str) -> [u8; 32] {
    sha2::Sha256::digest(password.as_bytes()).into()
}

pub fn random_vec(len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    rand::rng().fill_bytes(&mut buf);
    buf
}