// Let test binaries find libcef.so without LD_LIBRARY_PATH (dev builds).
fn main() {
    println!("cargo:rerun-if-env-changed=CEF_PATH");
    if let Ok(p) = std::env::var("CEF_PATH") {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
            println!("cargo:rustc-link-arg-tests=-Wl,-rpath,{p}");
        }
    }
}
