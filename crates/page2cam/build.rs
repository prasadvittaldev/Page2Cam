include!("../../packaging/windows/versioninfo.rs");

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    // Dev builds on Linux: find libcef.so without LD_LIBRARY_PATH.
    println!("cargo:rerun-if-env-changed=CEF_PATH");
    if target_os == "linux" {
        if let Ok(p) = std::env::var("CEF_PATH") {
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{p}");
        }
        // Packaged builds: libcef.so sits next to the binary.
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
    }
    // Windows: embed the application manifest (supported OS, DPI awareness),
    // which Chromium relies on for correct version detection and scaling, the
    // icon, and the version information (product name and version, which
    // code signing checks).
    if target_os == "windows" {
        let dir =
            std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("windows");
        for f in ["page2cam.exe.manifest", "page2cam.rc", "page2cam.ico"] {
            println!("cargo:rerun-if-changed={}", dir.join(f).display());
        }
        println!("cargo:rerun-if-changed=../../packaging/windows/versioninfo.rs");
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        // page2cam.rc with its files as absolute paths (forward slashes work
        // in both rc.exe and llvm-rc), plus the VERSIONINFO block.
        let abs = dir.display().to_string().replace('\\', "/");
        let rc = std::fs::read_to_string(dir.join("page2cam.rc"))
            .unwrap()
            .replace("\"page2cam.", &format!("\"{abs}/page2cam."));
        let full = out.join("page2cam-full.rc");
        std::fs::write(
            &full,
            format!("{rc}\n{}", version_rc("Page2Cam", "page2cam.exe")),
        )
        .unwrap();
        let res = out.join("page2cam.res");
        if compile_rc(&full, &res) {
            println!("cargo:rustc-link-arg-bins={}", res.display());
        } else {
            println!("cargo:warning=no resource compiler (llvm-rc/rc) found; page2cam.exe built without its manifest");
        }
    }
}
