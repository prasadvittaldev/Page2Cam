include!("../../packaging/windows/versioninfo.rs");

fn main() {
    // Windows: version information (product name and version, which code
    // signing checks) for the camera DLL.
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }
    println!("cargo:rerun-if-changed=../../packaging/windows/versioninfo.rs");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc = out.join("p2c_vcam_win_source.rc");
    std::fs::write(
        &rc,
        version_rc("Page2Cam virtual camera", "p2c_vcam_win_source.dll"),
    )
    .unwrap();
    let res = out.join("p2c_vcam_win_source.res");
    if compile_rc(&rc, &res) {
        println!("cargo:rustc-cdylib-link-arg={}", res.display());
    } else {
        println!("cargo:warning=no resource compiler (llvm-rc/rc) found; the camera DLL is built without version information");
    }
}
