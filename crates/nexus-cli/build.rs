//! Embed physical-pixel awareness in the shipped MSVC binary, independent of MSI packaging.
fn main() {
    println!("cargo:rerun-if-changed=../../installers/windows/nexus.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }
    let Some(root) = std::env::var_os("CARGO_MANIFEST_DIR") else {
        return;
    };
    let manifest = std::path::PathBuf::from(root).join("../../installers/windows/nexus.manifest");
    println!("cargo:rustc-link-arg-bin=nexus=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bin=nexus=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
