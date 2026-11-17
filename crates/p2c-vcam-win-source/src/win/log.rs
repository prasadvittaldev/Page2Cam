//! Tiny append-only log in C:\ProgramData\Page2Cam\vcam.log (the DLL runs in a
//! service, so there is no console). Never fails the caller.
use std::io::Write;

pub(crate) fn log(msg: &str) {
    let dir = std::env::var_os("ProgramData")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\ProgramData".into());
    let dir = dir.join("Page2Cam");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("vcam.log");
    if std::fs::metadata(&path)
        .map(|m| m.len() > 1_000_000)
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{secs}] pid {} {msg}", std::process::id());
    }
}
