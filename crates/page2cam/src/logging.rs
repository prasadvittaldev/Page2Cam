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

/// Log to the console and to `page2cam.log`: next to the exe if writable
/// (Windows zip folder), else in the app data dir. Returns the file in use.
pub fn init() -> Option<PathBuf> {
    let p = init_inner();
    let _ = PATH.set(p.clone());
    p
}

fn init_inner() -> Option<PathBuf> {
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let path = log_path();
    let file = path.as_ref().and_then(|p| {
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(p)
            .ok()
    });
    let console = fmt::layer().with_filter(filter());
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

fn log_path() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok()?.parent()?.join("page2cam.log");
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
    Some(dir.join("page2cam.log"))
}
