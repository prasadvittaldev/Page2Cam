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
    // which Chromium relies on for correct version detection and scaling.
    if target_os == "windows" {
        let dir =
            std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("windows");
        println!(
            "cargo:rerun-if-changed={}",
            dir.join("page2cam.exe.manifest").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            dir.join("page2cam.rc").display()
        );
        let res = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("page2cam.res");
        // llvm-rc (cross builds) or rc.exe (native MSVC builds).
        let compiled = ["llvm-rc", "rc"].iter().any(|rc| {
            std::process::Command::new(rc)
                .current_dir(&dir)
                .args(["/nologo", "/fo"])
                .arg(&res)
                .arg("page2cam.rc")
                .status()
                .is_ok_and(|s| s.success())
        });
        if compiled {
            println!("cargo:rustc-link-arg-bins={}", res.display());
        } else {
            println!("cargo:warning=no resource compiler (llvm-rc/rc) found; page2cam.exe built without its manifest");
        }
    }
}
