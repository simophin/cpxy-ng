//! Historical v1 experiment executable; production transport is cpxy-rtp.
fn main() -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported,"The v1 RTP experiment was replaced; use cpxy-rtp endpoint/client"))
}
