// Let the dev binary find libcef.so without LD_LIBRARY_PATH.
fn main() {
    println!("cargo:rerun-if-env-changed=CEF_PATH");
    if let Ok(p) = std::env::var("CEF_PATH") {
        if cfg!(target_os = "linux") {
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{p}");
        }
    }
}
