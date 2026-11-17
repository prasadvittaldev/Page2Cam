// Let test binaries find libcef.so without LD_LIBRARY_PATH (dev builds).
fn main() {
    println!("cargo:rerun-if-env-changed=CEF_PATH");
    if let Ok(p) = std::env::var("CEF_PATH") {
        if cfg!(target_os = "linux") {
            println!("cargo:rustc-link-arg-tests=-Wl,-rpath,{p}");
        }
    }
}
