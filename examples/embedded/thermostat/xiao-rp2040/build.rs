//! Put memory.x where the linker looks.
fn main() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::copy("memory.x", out.join("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");
}
