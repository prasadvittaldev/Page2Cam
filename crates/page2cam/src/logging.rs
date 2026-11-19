//! Log to the console and to a file, so a tester can send us the file.
use std::path::PathBuf;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Layer};

static PATH: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// The log file chosen by [`init`].
pub fn current_path() -> Option<PathBuf> {
    PATH.get().cloned().flatten()
}

/// Log to the console (if `console`) and to `page2cam.log`: next to the exe if writable
/// (Windows zip folder), else in the app data dir. Returns the file in use.
pub fn init(console: bool) -> Option<PathBuf> {
    init_named(console, "page2cam.log")
}

/// [`init`] with another file name (the tray writes `page2cam-tray.log`).
pub fn init_named(console: bool, name: &str) -> Option<PathBuf> {
    let p = init_inner(console, name);
    let _ = PATH.set(p.clone());
    p
}

fn init_inner(console: bool, name: &str) -> Option<PathBuf> {
    // ONNX Runtime logs ~300 info lines per model load; keep its warnings only.
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,ort=warn".into());
    let path = log_path(name);
    let file = path.as_ref().and_then(|p| {
        // Keep the previous run (e.g. the engine that just crashed).
        rotate(p);
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(p)
            .ok()
    });
    // Engine mode: no console output (the tray reads the log file).
    let console = console.then(|| fmt::layer().with_filter(filter()));
    match file {
        Some(f) => {
            let file_layer = fmt::layer()
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(f))
                .with_filter(filter());
            tracing_subscriber::registry()
                .with(console)
                .with(file_layer)
                .init();
            path
        }
        None => {
            tracing_subscriber::registry().with(console).init();
            None
        }
    }
}

fn log_path(name: &str) -> Option<PathBuf> {
    let beside = std::env::current_exe().ok()?.parent()?.join(name);
    if cfg!(windows)
        && std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&beside)
            .is_ok()
    {
        return Some(beside);
    }
    let dir = directories::ProjectDirs::from("", "", "page2cam")?
        .data_dir()
        .to_path_buf();
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(name))
}

/// `x.log` -> `x.prev.log` (replacing an older one) before a run truncates
/// it, so the log of the run before survives one restart. Empty files are
/// left alone (the writability probe in `log_path` may just have made one).
fn rotate(path: &std::path::Path) {
    if !std::fs::metadata(path).is_ok_and(|m| m.len() > 0) {
        return;
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "page2cam".into());
    let prev = path.with_file_name(format!("{stem}.prev.log"));
    // Replaces an older .prev.log (on Windows too: MoveFileEx REPLACE_EXISTING).
    if let Err(e) = std::fs::rename(path, &prev) {
        // Not fatal: this run's log then simply starts over.
        eprintln!("could not keep the previous log as {}: {e}", prev.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previous_log_is_kept_once() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("page2cam.log");
        let prev = dir.path().join("page2cam.prev.log");
        rotate(&log); // missing: nothing happens
        assert!(!prev.exists());
        std::fs::write(&log, "run 1").unwrap();
        rotate(&log);
        assert_eq!(std::fs::read_to_string(&prev).unwrap(), "run 1");
        assert!(!log.exists());
        std::fs::write(&log, "run 2").unwrap();
        rotate(&log);
        assert_eq!(std::fs::read_to_string(&prev).unwrap(), "run 2");
        // An empty log (just created by the probe) does not replace it.
        std::fs::write(&log, "").unwrap();
        rotate(&log);
        assert_eq!(std::fs::read_to_string(&prev).unwrap(), "run 2");
        // The tray's log gets its own previous file.
        let tray = dir.path().join("page2cam-tray.log");
        std::fs::write(&tray, "tray").unwrap();
        rotate(&tray);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("page2cam-tray.prev.log")).unwrap(),
            "tray"
        );
    }
}
