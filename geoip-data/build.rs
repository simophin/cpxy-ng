use sha2::{Digest, Sha256};
use std::path::PathBuf;

const SOURCE_PATH: &str = "data/geoip.dat";
const EXPECTED_SHA256: &str = "150dba0a921e014d6284b4fdb14803aca9e585e6cad41022e9be0f07ee454683";

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo must provide CARGO_MANIFEST_DIR"),
    );
    let source = manifest_dir.join(SOURCE_PATH);
    println!("cargo:rerun-if-changed={}", source.display());

    let bytes = std::fs::read(&source)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", source.display()));
    let actual_sha256 = format!("{:x}", Sha256::digest(&bytes));
    assert_eq!(
        actual_sha256,
        EXPECTED_SHA256,
        "{} does not match the reviewed GeoIP input; follow SOURCE.md when updating it",
        source.display(),
    );

    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR"))
        .join("geoip.dat");
    std::fs::write(&output, bytes)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", output.display()));
}
