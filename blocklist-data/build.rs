use std::path::{Path, PathBuf};

#[path = "src/compile.rs"]
mod compile;

const SOURCE_URL: &str =
    "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/multi.txt";
/// Where the downloaded list is kept, so local builds download it only once. Not committed, so
/// every CI checkout downloads the latest one. Delete it to refresh.
const CACHE_PATH: &str = "data/hagezi-multi.txt";
/// Fewer names than this means the list was not downloaded properly.
const MIN_NAMES: usize = 10_000;

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo must provide CARGO_MANIFEST_DIR"),
    );
    let cache = manifest_dir.join(CACHE_PATH);
    // A missing file counts as changed, so Cargo reruns this script after the cache is deleted.
    println!("cargo:rerun-if-changed={}", cache.display());
    println!("cargo:rerun-if-changed=src/compile.rs");

    let (text, downloaded) = match std::fs::read_to_string(&cache) {
        Ok(text) => (text, false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (download(), true),
        Err(error) => panic!("failed to read {}: {error}", cache.display()),
    };

    let compiled = compile::compile(&text).unwrap_or_else(|error| panic!("{SOURCE_URL}: {error}"));
    let names = compiled.index.len() / 4 - 1;
    assert!(
        names >= MIN_NAMES,
        "{SOURCE_URL} has only {names} names; delete {} to download it again",
        cache.display(),
    );
    // Only a list that compiled is cached, so a bad download is retried on the next build.
    if downloaded {
        save(&cache, &text);
    }

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR"));
    for (file, bytes) in [
        ("names.bin", &compiled.names),
        ("index.bin", &compiled.index),
    ] {
        let output = out_dir.join(file);
        std::fs::write(&output, bytes)
            .unwrap_or_else(|error| panic!("failed to write {}: {error}", output.display()));
    }
}

fn download() -> String {
    ureq::get(SOURCE_URL)
        .call()
        .and_then(|response| {
            response
                .into_body()
                .with_config()
                .limit(64 * 1024 * 1024)
                .read_to_string()
        })
        .unwrap_or_else(|error| panic!("failed to download {SOURCE_URL}: {error}"))
}

/// Writes through a temporary file, as builds for several targets may download at the same time.
fn save(cache: &Path, text: &str) {
    let dir = cache.parent().expect("the cache path has a directory");
    std::fs::create_dir_all(dir)
        .unwrap_or_else(|error| panic!("failed to create {}: {error}", dir.display()));
    let temp = cache.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temp, text)
        .and_then(|()| std::fs::rename(&temp, cache))
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", cache.display()));
}
