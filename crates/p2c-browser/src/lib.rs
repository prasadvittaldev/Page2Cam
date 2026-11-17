//! Off-screen Chromium (CEF) rendering into a FrameSlot.
//!
//! CEF must own the process's main thread: call [`maybe_run_subprocess`] first
//! thing in `main`, then [`run`] on the main thread. Everything else talks to
//! the browser through a [`BrowserHandle`].
mod handlers;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cef::args::Args;
use cef::{CefString, ImplBrowser, ImplBrowserHost, ImplCommandLine, ImplFrame};
use p2c_core::FrameSlot;

#[derive(Clone, Debug)]
pub struct BrowserConfig {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Persistent profile (cookies, logins) directory.
    pub profile_dir: PathBuf,
}

enum Cmd {
    Navigate(String),
    Shutdown,
}

/// Thread-safe control handle; commands run on the CEF main thread.
#[derive(Clone)]
pub struct BrowserHandle {
    tx: Sender<Cmd>,
}

impl BrowserHandle {
    pub fn navigate(&self, url: &str) {
        let _ = self.tx.send(Cmd::Navigate(url.to_owned()));
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(Cmd::Shutdown);
    }
}

/// If this process is a CEF helper subprocess (renderer, GPU, ...), run it and
/// return its exit code. Returns `None` in the main browser process.
pub fn maybe_run_subprocess() -> Option<i32> {
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = Args::new();
    let is_browser = args
        .as_cmd_line()
        .map(|c| c.has_switch(Some(&CefString::from("type"))) != 1)
        .unwrap_or(true);
    if is_browser {
        return None;
    }
    let mut app = handlers::app();
    Some(cef::execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    ))
}

/// Start CEF, create one off-screen browser and pump its message loop on the
/// calling thread until [`BrowserHandle::shutdown`]. Every painted view frame
/// (always the full buffer) is put into `slot`.
pub fn run(
    cfg: BrowserConfig,
    slot: FrameSlot,
    ready: impl FnOnce(BrowserHandle) + Send + 'static,
) -> anyhow::Result<()> {
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    std::fs::create_dir_all(&cfg.profile_dir)?;
    let args = Args::new();
    let mut app = handlers::app();
    let settings = cef::Settings {
        windowless_rendering_enabled: 1,
        no_sandbox: 1,
        root_cache_path: CefString::from(cfg.profile_dir.to_string_lossy().as_ref()),
        log_severity: cef::LogSeverity::WARNING,
        ..Default::default()
    };
    anyhow::ensure!(
        cef::initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(&mut app),
            std::ptr::null_mut()
        ) == 1,
        "CEF failed to initialize (is CEF_PATH/LD_LIBRARY_PATH set and the CEF version matching?)"
    );

    let closed = Arc::new(AtomicBool::new(false));
    let mut client = handlers::client(slot, cfg.width, cfg.height, closed.clone());
    let window_info = cef::WindowInfo {
        windowless_rendering_enabled: 1,
        ..Default::default()
    };
    let browser_settings = cef::BrowserSettings {
        windowless_frame_rate: cfg.fps as _,
        ..Default::default()
    };
    let browser = cef::browser_host_create_browser_sync(
        Some(&window_info),
        Some(&mut client),
        Some(&CefString::from(cfg.url.as_str())),
        Some(&browser_settings),
        None,
        None,
    );
    let Some(browser) = browser else {
        cef::shutdown();
        anyhow::bail!("CEF failed to create the browser");
    };

    let (tx, rx): (Sender<Cmd>, Receiver<Cmd>) = mpsc::channel();
    ready(BrowserHandle { tx });

    // Pump CEF from this thread; poll commands between pumps.
    let mut closing_since: Option<Instant> = None;
    loop {
        cef::do_message_loop_work();
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Navigate(url) => {
                    if let Some(frame) = browser.main_frame() {
                        frame.load_url(Some(&CefString::from(url.as_str())));
                    }
                }
                Cmd::Shutdown if closing_since.is_none() => {
                    if let Some(host) = browser.host() {
                        host.close_browser(1);
                    }
                    closing_since = Some(Instant::now());
                }
                Cmd::Shutdown => {}
            }
        }
        if let Some(t) = closing_since {
            if closed.load(Ordering::SeqCst) || t.elapsed() > Duration::from_secs(3) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(browser);
    // Let CEF release the closed browser before shutting down.
    for _ in 0..10 {
        cef::do_message_loop_work();
    }
    cef::shutdown();
    Ok(())
}

pub(crate) fn add_switches(cmd: &mut cef::CommandLine) {
    for s in [
        "noerrdialogs",
        "hide-crash-restore-bubble",
        "disable-session-crashed-bubble",
        "use-mock-keychain",
    ] {
        cmd.append_switch(Some(&CefString::from(s)));
    }
    // Pages with video should start playing without a click.
    cmd.append_switch_with_value(
        Some(&CefString::from("autoplay-policy")),
        Some(&CefString::from("no-user-gesture-required")),
    );
}
