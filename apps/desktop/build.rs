fn main() {
    // Тип сборки попадает в журнал и настройки: `dev` или `release`.
    println!("cargo:rerun-if-env-changed=UTIL_MAKER_BUILD");
    let build_kind = std::env::var("UTIL_MAKER_BUILD").unwrap_or_else(|_| "dev".to_owned());
    println!("cargo:rustc-env=UTIL_MAKER_BUILD={build_kind}");

    // Номер сборки задаётся в окружении: BUILD_NUMBER=42 make build-windows.
    println!("cargo:rerun-if-env-changed=UTIL_MAKER_BUILD_NUMBER");
    let build_number = std::env::var("UTIL_MAKER_BUILD_NUMBER").unwrap_or_else(|_| "0".to_owned());
    println!("cargo:rustc-env=UTIL_MAKER_BUILD_NUMBER={build_number}");

    // MSVC CRT из cargo-xwin поставляется без PDB: lld-link пишет LNK4099 на
    // каждую статическую библиотеку. Это не влияет на код, поэтому для
    // Windows-цели предупреждение подавляется.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg=/IGNORE:4099");
    }
    tauri_build::build();
}
