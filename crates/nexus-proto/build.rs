//! Compiles `proto/nexus/v1/*.proto` with the pure-Rust `protox` compiler, so building the
//! workspace never requires a system `protoc` (one fewer thing to install on CI runners).

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = PathBuf::from("proto");
    let files: Vec<PathBuf> = ["common.proto", "session.proto", "signal.proto", "ipc.proto"]
        .iter()
        .map(|f| proto_root.join("nexus/v1").join(f))
        .collect();

    let descriptors = protox::compile(&files, [&proto_root])?;
    prost_build::Config::new()
        // `Bytes` instead of `Vec<u8>` for `bytes` fields: decoding then slices the receive
        // buffer instead of copying (matters for clipboard images and file chunks).
        .bytes(["."])
        .compile_fds(descriptors)?;

    println!("cargo:rerun-if-changed=proto");
    Ok(())
}
