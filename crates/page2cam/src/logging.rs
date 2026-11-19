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
