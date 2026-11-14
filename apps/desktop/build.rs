fn main() {
    // MSVC CRT из cargo-xwin поставляется без PDB: lld-link пишет LNK4099 на
    // каждую статическую библиотеку. Это не влияет на код, поэтому для
    // Windows-цели предупреждение подавляется.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg=/IGNORE:4099");
    }
    tauri_build::build();
}
