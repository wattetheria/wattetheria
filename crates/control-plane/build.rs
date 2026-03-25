use std::path::{Path, PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let proto_path = wattswarm_sync_proto_path()?;
    let include_dir = proto_path
        .parent()
        .ok_or("wattswarm sync proto missing parent directory")?;
    let include_dir = include_dir.to_path_buf();

    println!("cargo:rerun-if-changed={}", proto_path.display());

    let mut config = prost_build::Config::new();
    config.protoc_executable(protoc);
    tonic_build::configure().compile_protos_with_config(config, &[proto_path], &[include_dir])?;
    Ok(())
}

fn wattswarm_sync_proto_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = std::env::var_os("WATTSWARM_SYNC_PROTO") {
        return Ok(PathBuf::from(path));
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let repo_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .ok_or("resolve wattetheria repo root")?;
    Ok(repo_root
        .parent()
        .ok_or("resolve parent workspace root")?
        .join("wattswarm/apps/wattswarm/proto/wattetheria_sync.proto"))
}
