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
use p2c_core::{AudioRing, FrameSlot};
use parking_lot::Mutex;

#[derive(Clone, Debug)]
pub struct BrowserConfig {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Persistent profile (cookies, logins) directory.
    pub profile_dir: PathBuf,
    /// Where page audio goes (48 kHz stereo). `None`: the page plays to speakers.
    pub audio: Option<AudioRing>,
    /// GPU path (Windows): Chromium renders on this adapter and hands each
    /// frame over as a shared texture instead of a CPU buffer. `None`: CPU
    /// paints into the `FrameSlot` (Linux, CPU mode).
    pub gpu_page: Option<GpuPage>,
}

/// Chromium on a chosen GPU, delivering shared textures.
#[derive(Clone)]
pub struct GpuPage {
    /// The adapter's LUID (`(HighPart as u32 as u64) << 32 | LowPart`). It
    /// **must** come from a DXGI enumeration made in this process run: a LUID
    /// that matches no adapter makes Chromium's GPU process crash and CEF exit.
    pub adapter_luid: u64,
    /// Called on the CEF UI thread with each frame's shared NT handle (valid
    /// only during the call) and size. Must copy quickly and never block on
    /// the compositor. Returns false when the texture could not be opened.
    pub on_texture: Arc<dyn Fn(isize, u32, u32) -> bool + Send + Sync>,
}

impl std::fmt::Debug for GpuPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuPage")
            .field("adapter_luid", &format_args!("{:#x}", self.adapter_luid))
            .finish_non_exhaustive()
    }
}

/// The `use-adapter-luid` switch value: `<HighPart>,<LowPart>` in decimal,
/// HighPart signed (Chromium parses them with StringToInt / StringToUint;
/// hex is not accepted).
pub fn luid_switch_value(luid: u64) -> String {
    format!("{},{}", (luid >> 32) as u32 as i32, luid as u32)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    /// Key pressed (CEF RAWKEYDOWN).
    Down,
    Up,
    /// Text input for a printable character.
    Char,
}

/// A keyboard event. `windows_key_code` is a Windows virtual-key code (what
/// the DOM's `keyCode` reports); `character` is the UTF-16 unit for `Char`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub kind: KeyKind,
    pub windows_key_code: i32,
    pub character: u16,
    /// CEF `EVENTFLAG_*` bits (shift 2, ctrl 4, alt 8, ...).
    pub modifiers: u32,
}

enum Cmd {
    Navigate(String),
    Back,
    Forward,
    Reload,
    MouseMove {
        x: i32,
        y: i32,
        mods: u32,
    },
    MouseButton {
        x: i32,
        y: i32,
        button: MouseButton,
        down: bool,
        clicks: i32,
        mods: u32,
    },
    Wheel {
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
        mods: u32,
    },
    Key(KeyInput),
    RecreateCpu,
    Shutdown,
}

/// Thread-safe control handle; commands run on the CEF main thread.
/// Coordinates are page pixels.
#[derive(Clone)]
pub struct BrowserHandle {
    tx: Sender<Cmd>,
    url: Arc<Mutex<String>>,
}

impl BrowserHandle {
    fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }

    pub fn navigate(&self, url: &str) {
        self.send(Cmd::Navigate(url.to_owned()));
    }

    pub fn back(&self) {
        self.send(Cmd::Back);
    }

    pub fn forward(&self) {
        self.send(Cmd::Forward);
    }

    pub fn reload(&self) {
        self.send(Cmd::Reload);
    }

    pub fn mouse_move(&self, x: i32, y: i32, mods: u32) {
        self.send(Cmd::MouseMove { x, y, mods });
    }

    pub fn mouse_button(
        &self,
        x: i32,
        y: i32,
        button: MouseButton,
        down: bool,
        clicks: i32,
        mods: u32,
    ) {
        self.send(Cmd::MouseButton {
            x,
            y,
            button,
            down,
            clicks,
            mods,
        });
    }

    pub fn wheel(&self, x: i32, y: i32, dx: i32, dy: i32, mods: u32) {
        self.send(Cmd::Wheel { x, y, dx, dy, mods });
    }

    pub fn key(&self, k: KeyInput) {
        self.send(Cmd::Key(k));
    }

    /// URL of the main frame as last reported by the browser.
    pub fn current_url(&self) -> String {
        self.url.lock().clone()
    }

    pub fn shutdown(&self) {
        self.send(Cmd::Shutdown);
    }

    /// CPU fallback without re-initialising CEF: replace the browser with one
    /// that paints into the `FrameSlot` (`shared_texture_enabled = 0`), on the
    /// current URL. Does nothing if the browser already paints on the CPU.
    pub fn recreate_cpu(&self) {
        self.send(Cmd::RecreateCpu);
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
    let mut app = handlers::app(None);
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
    // Shared textures are a Windows (D3D11) feature here; elsewhere CPU paints.
    let gpu_page = if cfg!(windows) {
        cfg.gpu_page.clone()
    } else {
        if cfg.gpu_page.is_some() {
            tracing::warn!("GPU page textures are Windows-only; using CPU paints");
        }
        None
    };
    let adapter_luid = gpu_page.as_ref().map(|g| luid_switch_value(g.adapter_luid));
    if let Some(v) = &adapter_luid {
        tracing::info!(use_adapter_luid = %v, "Chromium on the chosen GPU, shared textures");
    }
    let mut app = handlers::app(adapter_luid);
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

    let url = Arc::new(Mutex::new(cfg.url.clone()));
    let create = |gpu: Option<&GpuPage>, at: &str| {
        // Each browser has its own `closed` flag, so closing a replaced
        // browser doesn't end the pump loop.
        let closed = Arc::new(AtomicBool::new(false));
        let mut client = handlers::client(handlers::ClientParts {
            slot: slot.clone(),
            width: cfg.width,
            height: cfg.height,
            closed: closed.clone(),
            url: url.clone(),
            audio: cfg.audio.clone(),
            on_texture: gpu.map(|g| g.on_texture.clone()),
        });
        let window_info = cef::WindowInfo {
            windowless_rendering_enabled: 1,
            shared_texture_enabled: gpu.is_some() as _,
            ..Default::default()
        };
        let browser_settings = cef::BrowserSettings {
            windowless_frame_rate: cfg.fps as _,
            ..Default::default()
        };
        cef::browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut client),
            Some(&CefString::from(at)),
            Some(&browser_settings),
            None,
            None,
        )
        .map(|b| (b, closed))
    };
    let Some((mut browser, mut closed)) = create(gpu_page.as_ref(), &cfg.url) else {
        cef::shutdown();
        anyhow::bail!("CEF failed to create the browser");
    };
    let mut on_gpu = gpu_page.is_some();

    let (tx, rx): (Sender<Cmd>, Receiver<Cmd>) = mpsc::channel();
    ready(BrowserHandle {
        tx,
        url: url.clone(),
    });

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
                Cmd::Back => browser.go_back(),
                Cmd::Forward => browser.go_forward(),
                Cmd::Reload => browser.reload(),
                Cmd::MouseMove { x, y, mods } => {
                    if let Some(host) = browser.host() {
                        host.send_mouse_move_event(Some(&mouse(x, y, mods)), 0);
                    }
                }
                Cmd::MouseButton {
                    x,
                    y,
                    button,
                    down,
                    clicks,
                    mods,
                } => {
                    if let Some(host) = browser.host() {
                        host.set_focus(1);
                        let b = match button {
                            MouseButton::Left => cef::MouseButtonType::LEFT,
                            MouseButton::Middle => cef::MouseButtonType::MIDDLE,
                            MouseButton::Right => cef::MouseButtonType::RIGHT,
                        };
                        host.send_mouse_click_event(
                            Some(&mouse(x, y, mods)),
                            b,
                            (!down) as _,
                            clicks,
                        );
                    }
                }
                Cmd::Wheel { x, y, dx, dy, mods } => {
                    if let Some(host) = browser.host() {
                        host.send_mouse_wheel_event(Some(&mouse(x, y, mods)), dx, dy);
                    }
                }
                Cmd::Key(k) => {
                    if let Some(host) = browser.host() {
                        host.send_key_event(Some(&key_event(k)));
                    }
                }
                Cmd::RecreateCpu if on_gpu && closing_since.is_none() => {
                    // New browser first: if it can't be made, keep the old one.
                    let at = url.lock().clone();
                    match create(None, &at) {
                        Some((b, c)) => {
                            if let Some(host) = browser.host() {
                                host.close_browser(1);
                            }
                            browser = b;
                            closed = c;
                            on_gpu = false;
                            tracing::info!(url = %at, "browser recreated with CPU paints");
                        }
                        None => tracing::error!("could not recreate the browser with CPU paints"),
                    }
                }
                Cmd::RecreateCpu => {}
                Cmd::Shutdown if closing_since.is_none() => {
                    if let Some(host) = browser.host() {
                        host.close_browser(1);
                    }
                    closing_since = Some(Instant::now());
                }
                Cmd::Shutdown => {}
            }
        }
        if should_exit(closed.load(Ordering::SeqCst), closing_since, Instant::now()) {
            if !closed.load(Ordering::SeqCst) {
                tracing::warn!("browser did not close within 3 s; shutting down anyway");
            }
            break;
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

fn mouse(x: i32, y: i32, modifiers: u32) -> cef::MouseEvent {
    cef::MouseEvent { x, y, modifiers }
}

/// CEF expects the character in `windows_key_code` for CHAR events on Windows
/// (like WM_CHAR), but the virtual-key code on Linux (like cefclient's GTK path).
fn key_code_for(k: KeyInput) -> i32 {
    if k.kind == KeyKind::Char && (cfg!(windows) || k.windows_key_code == 0) {
        k.character as i32
    } else {
        k.windows_key_code
    }
}

fn key_event(k: KeyInput) -> cef::KeyEvent {
    cef::KeyEvent {
        type_: match k.kind {
            KeyKind::Down => cef::KeyEventType::RAWKEYDOWN,
            KeyKind::Up => cef::KeyEventType::KEYUP,
            KeyKind::Char => cef::KeyEventType::CHAR,
        },
        modifiers: k.modifiers,
        windows_key_code: key_code_for(k),
        character: k.character,
        unmodified_character: k.character,
        ..Default::default()
    }
}

/// Leave the pump loop once the browser has closed, whatever closed it
/// (our Shutdown, `window.close()`, Chromium's own signal handling), or 3 s
/// after we asked it to close.
fn should_exit(closed: bool, closing_since: Option<Instant>, now: Instant) -> bool {
    closed || closing_since.is_some_and(|t| now.duration_since(t) > Duration::from_secs(3))
}

/// `adapter_luid`: value for `use-adapter-luid` (see [`luid_switch_value`]).
pub(crate) fn add_switches(cmd: &mut cef::CommandLine, adapter_luid: Option<&str>) {
    if let Some(v) = adapter_luid {
        cmd.append_switch_with_value(
            Some(&CefString::from("use-adapter-luid")),
            Some(&CefString::from(v)),
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exits_when_browser_closed_without_shutdown_command() {
        let now = Instant::now();
        assert!(should_exit(true, None, now));
    }

    #[test]
    fn keeps_running_while_open() {
        assert!(!should_exit(false, None, Instant::now()));
    }

    #[test]
    fn luid_switch_is_decimal_high_comma_low() {
        // The spike's NVIDIA and Intel LUIDs (hex is rejected by Chromium).
        assert_eq!(luid_switch_value(0x0000_0000_0002_c996), "0,182678");
        assert_eq!(luid_switch_value(0x0000_0000_0002_c3e9), "0,181225");
        // HighPart is signed (LONG), LowPart unsigned (DWORD).
        assert_eq!(luid_switch_value(0xffff_ffff_8000_0000), "-1,2147483648");
        assert_eq!(luid_switch_value(0x0000_0001_ffff_ffff), "1,4294967295");
    }

    #[test]
    fn closing_waits_then_gives_up_after_3s() {
        let t = Instant::now();
        assert!(!should_exit(false, Some(t), t + Duration::from_secs(1)));
        assert!(should_exit(false, Some(t), t + Duration::from_secs(4)));
    }
}
