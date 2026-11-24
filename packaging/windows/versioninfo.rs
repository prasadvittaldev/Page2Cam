/// A Windows VERSIONINFO resource for this crate's binary (product name
/// "Page2Cam", the workspace version), as `.rc` text.
pub fn version_rc(description: &str, original_name: &str) -> String {
    let v = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut n: Vec<u32> = v
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    n.resize(4, 0);
    let commas = format!("{},{},{},{}", n[0], n[1], n[2], n[3]);
    let dotted = format!("{}.{}.{}.{}", n[0], n[1], n[2], n[3]);
    format!(
        r#"1 VERSIONINFO
FILEVERSION {commas}
PRODUCTVERSION {commas}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE {filetype}
FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Prasad Vittaldev"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{dotted}"
      VALUE "InternalName", "{original_name}"
      VALUE "LegalCopyright", "GPL-3.0-or-later"
      VALUE "OriginalFilename", "{original_name}"
      VALUE "ProductName", "Page2Cam"
      VALUE "ProductVersion", "{v}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        filetype = if original_name.ends_with(".dll") { "0x2L" } else { "0x1L" },
    )
}

/// Compile `rc` to `res` with llvm-rc (cross builds) or rc.exe (native MSVC
/// builds). False when neither is there or both fail.
pub fn compile_rc(rc: &std::path::Path, res: &std::path::Path) -> bool {
    ["llvm-rc", "rc"].iter().any(|tool| {
        std::process::Command::new(tool)
            .args(["/nologo", "/fo"])
            .arg(res)
            .arg(rc)
            .status()
            .is_ok_and(|s| s.success())
    })
}
