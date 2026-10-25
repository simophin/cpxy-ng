use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// The reviewed files under `data/`, with their SHA-256.
const SOURCES: &[(&str, &str)] = &[
    (
        "cn.dat",
        "e7059375cb244a9391df14b90782c34c10325321275d48cfe8e357649da17140",
    ),
    (
        "cn6.dat",
        "f914890841dc805dd60fb82295b5a5b826fa9d7317a541d6740ae2baf006fdab",
    ),
];

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo must provide CARGO_MANIFEST_DIR"),
    );
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR"));

    for (name, expected_sha256) in SOURCES {
        if *name == "cn.dat" {
            println!("cargo:rustc-env=CPXY_GEOIP_V4_SHA256={expected_sha256}");
        }
        let source = manifest_dir.join("data").join(name);
        println!("cargo:rerun-if-changed={}", source.display());

        let bytes = std::fs::read(&source)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", source.display()));
        let actual_sha256 = format!("{:x}", Sha256::digest(&bytes));
        assert_eq!(
            actual_sha256,
            *expected_sha256,
            "{} does not match the reviewed GeoIP input; follow SOURCE.md when updating it",
            source.display(),
        );

        let output = out_dir.join(name);
        std::fs::write(&output, bytes)
            .unwrap_or_else(|error| panic!("failed to write {}: {error}", output.display()));
    }
}
