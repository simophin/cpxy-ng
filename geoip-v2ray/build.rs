use std::io::Result;
fn main() -> Result<()> {
    let protoc_path = protoc_bin_vendored::protoc_bin_path().unwrap();

    prost_build::Config::new()
        .protoc_executable(protoc_path)
        .compile_protos(&["src/geoip.proto"], &["src/"])?;

    Ok(())
}
