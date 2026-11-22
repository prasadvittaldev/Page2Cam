//! Off-screen Chromium (CEF) rendering into a FrameSlot.
//!
//! CEF must own the process's main thread: call [`maybe_run_subprocess`] first
//! thing in `main`, then [`run`] on the main thread. Everything else talks to
//! the browser through a [`BrowserHandle`].
mod handlers;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
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
    /// While set, the page's paints are dropped (another source, e.g.
    /// screen capture, fills the page). Shared with the caller, so it can be
    /// set before the browser exists; [`BrowserHandle::set_paused`] sets it too.
    pub paused: Arc<AtomicBool>,
    /// A second, transparent browser in the same CEF instance (overlay
    /// layers). Reached through [`BrowserHandle::overlay`].
    pub overlay: Option<OverlayBrowser>,
}

/// Receives a shared texture: NT handle (valid only during the call), width, height.
pub type TextureCallback = Arc<dyn Fn(isize, u32, u32) -> bool + Send + Sync>;

/// The transparent overlay browser: its background is fully transparent, so
/// its frames carry premultiplied alpha (0 where nothing is drawn). It
/// renders at the page's configured size (never resized) on the same GPU.
#[derive(Clone)]
pub struct OverlayBrowser {
    pub url: String,
    /// CPU paints go here (CPU path, and after a CPU fallback).
    pub slot: FrameSlot,
    pub fps: u32,
    /// Where its sound goes (48 kHz stereo); `None` plays to the speakers.
    pub audio: Option<AudioRing>,
    /// GPU path: shared textures (only used when the page is on the GPU).
    pub on_texture: Option<TextureCallback>,
    /// Paints are dropped while set (nothing visible: no overlay pass).
    pub paused: Arc<AtomicBool>,
}

impl std::fmt::Debug for OverlayBrowser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayBrowser")
            .field("url", &self.url)
            .field("fps", &self.fps)
            .field("gpu", &self.on_texture.is_some())
            .finish_non_exhaustive()
    }
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
    Repaint,
    RecreateCpu,
    SetFrameRate(u32),
    /// The new size is already in the shared size cell.
    SetSize,
    Shutdown,
}

/// Which browser a command is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Page,
    Overlay,
}

/// Thread-safe control handle; commands run on the CEF main thread.
/// Coordinates are page pixels.
#[derive(Clone)]
pub struct BrowserHandle {
    tx: Sender<(Target, Cmd)>,
    target: Target,
    url: Arc<Mutex<String>>,
    paused: Arc<AtomicBool>,
    size: Arc<Mutex<(u32, u32)>>,
    overlay: Option<Arc<BrowserHandle>>,
}

impl BrowserHandle {
    fn send(&self, c: Cmd) {
        let _ = self.tx.send((self.target, c));
    }

    /// The transparent overlay browser's handle (`None` unless
    /// [`BrowserConfig::overlay`] was set). Its `shutdown` and
    /// `recreate_cpu` act on both browsers.
    pub fn overlay(&self) -> Option<BrowserHandle> {
        self.overlay.as_deref().cloned()
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

    /// Close the browser(s) and leave [`run`].
    pub fn shutdown(&self) {
        self.send(Cmd::Shutdown);
    }

    /// Make Chromium paint the whole page again, even if nothing changed
    /// (e.g. a static page whose frames went to a device that couldn't
    /// open them).
    pub fn repaint(&self) {
        self.send(Cmd::Repaint);
    }

    /// CPU fallback without re-initialising CEF: replace every browser that
    /// hands over shared textures with one that paints into its `FrameSlot`
    /// (`shared_texture_enabled = 0`), on its current URL, frame rate and
    /// size. Does nothing for a browser that already paints on the CPU.
    pub fn recreate_cpu(&self) {
        self.send(Cmd::RecreateCpu);
    }

    /// Drop the page's paints while another source fills the page slot or
    /// texture (screen capture); `false` resumes and asks for a full repaint.
    pub fn set_paused(&self, paused: bool) {
        if self.paused.swap(paused, Ordering::SeqCst) && !paused {
            self.repaint();
        }
    }

    /// Whether paints are being dropped ([`BrowserHandle::set_paused`]).
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Change how often Chromium paints the page (frames per second, at
    /// least 1); also kept for a browser recreated later.
    pub fn set_frame_rate(&self, fps: u32) {
        self.send(Cmd::SetFrameRate(fps.max(1)));
    }

    /// Render the page at `w` x `h` view pixels (at least 16 x 16; the
    /// compositors scale it to the output). Also kept for a browser
    /// recreated later. Input coordinates are in this size from then on.
    pub fn set_size(&self, w: u32, h: u32) {
        let s = (w.max(16), h.max(16));
        if std::mem::replace(&mut *self.size.lock(), s) != s {
            self.send(Cmd::SetSize);
        }
    }

    /// The view size the page renders at (see [`BrowserHandle::set_size`]).
    pub fn size(&self) -> (u32, u32) {
        *self.size.lock()
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

/// What one browser is made from (kept to recreate it).
struct Spec {
    slot: FrameSlot,
    audio: Option<AudioRing>,
    on_texture: Option<handlers::TextureFn>,
    paused: Arc<AtomicBool>,
    url: Arc<Mutex<String>>,
    size: Arc<Mutex<(u32, u32)>>,
    /// Transparent background (the overlay browser).
    transparent: bool,
}

/// A live browser and its state.
struct Live {
    spec: Spec,
    browser: cef::Browser,
    closed: Arc<AtomicBool>,
    on_gpu: bool,
    fps: u32,
}

/// Create a windowless browser for `spec` at `at` (shared textures when
/// `gpu` and the spec has a texture callback).
fn create(spec: &Spec, gpu: bool, at: &str, fps: u32) -> Option<(cef::Browser, Arc<AtomicBool>)> {
    let gpu = gpu && spec.on_texture.is_some();
    // Each browser has its own `closed` flag, so closing a replaced
    // browser doesn't end the pump loop.
    let closed = Arc::new(AtomicBool::new(false));
    let mut client = handlers::client(handlers::ClientParts {
        slot: spec.slot.clone(),
        size: spec.size.clone(),
        closed: closed.clone(),
        url: spec.url.clone(),
        audio: spec.audio.clone(),
        on_texture: if gpu { spec.on_texture.clone() } else { None },
        paused: spec.paused.clone(),
    });
    let window_info = cef::WindowInfo {
        windowless_rendering_enabled: 1,
        shared_texture_enabled: gpu as _,
        ..Default::default()
    };
    let mut browser_settings = cef::BrowserSettings {
        windowless_frame_rate: fps as _,
        ..Default::default()
    };
    if spec.transparent {
        // ARGB 0: a fully transparent background (frames carry alpha).
        browser_settings.background_color = 0;
    }
    cef::browser_host_create_browser_sync(
        Some(&window_info),
        Some(&mut client),
        Some(&CefString::from(at)),
        Some(&browser_settings),
        None,
        None,
    )
    .map(|b| (b, closed))
}

impl Live {
    fn host(&self) -> Option<cef::BrowserHost> {
        self.browser.host()
    }

    /// Replace a shared-texture browser with a CPU-painting one (same URL,
    /// frame rate and size). New browser first: if it can't be made, keep
    /// the old one.
    fn recreate_cpu(&mut self, what: &str) {
        if !self.on_gpu {
            return;
        }
        let at = self.spec.url.lock().clone();
        match create(&self.spec, false, &at, self.fps) {
            Some((b, c)) => {
                if let Some(host) = self.host() {
                    host.close_browser(1);
                }
                self.browser = b;
                self.closed = c;
                self.on_gpu = false;
                tracing::info!(url = %at, "{what} browser recreated with CPU paints");
            }
            None => tracing::error!("could not recreate the {what} browser with CPU paints"),
        }
    }

    fn apply(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Navigate(url) => {
                if let Some(frame) = self.browser.main_frame() {
                    frame.load_url(Some(&CefString::from(url.as_str())));
                }
            }
            Cmd::Back => self.browser.go_back(),
            Cmd::Forward => self.browser.go_forward(),
            Cmd::Reload => self.browser.reload(),
            Cmd::MouseMove { x, y, mods } => {
                if let Some(host) = self.host() {
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
                if let Some(host) = self.host() {
                    host.set_focus(1);
                    let b = match button {
                        MouseButton::Left => cef::MouseButtonType::LEFT,
                        MouseButton::Middle => cef::MouseButtonType::MIDDLE,
                        MouseButton::Right => cef::MouseButtonType::RIGHT,
                    };
                    host.send_mouse_click_event(Some(&mouse(x, y, mods)), b, (!down) as _, clicks);
                }
            }
            Cmd::Wheel { x, y, dx, dy, mods } => {
                if let Some(host) = self.host() {
                    host.send_mouse_wheel_event(Some(&mouse(x, y, mods)), dx, dy);
                }
            }
            Cmd::Key(k) => {
                if let Some(host) = self.host() {
                    host.send_key_event(Some(&key_event(k)));
                }
            }
            Cmd::Repaint => {
                if let Some(host) = self.host() {
                    host.invalidate(cef::PaintElementType::default()); // the view
                }
            }
            Cmd::SetFrameRate(f) => {
                self.fps = f;
                if let Some(host) = self.host() {
                    host.set_windowless_frame_rate(f as _);
                }
            }
            Cmd::SetSize => {
                // The size is already in `spec.size` (view_rect reads it).
                if let Some(host) = self.host() {
                    host.was_resized();
                    host.invalidate(cef::PaintElementType::default());
                }
            }
            // Handled by the pump loop.
            Cmd::RecreateCpu | Cmd::Shutdown => {}
        }
    }
}

/// Start CEF, create the off-screen browser (and the overlay browser, if
/// configured) and pump the message loop on the calling thread until
/// [`BrowserHandle::shutdown`]. Every painted view frame (always the full
/// buffer) is put into `slot`.
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

    let on_gpu = gpu_page.is_some();
    let page_spec = Spec {
        slot: slot.clone(),
        audio: cfg.audio.clone(),
        on_texture: gpu_page.as_ref().map(|g| g.on_texture.clone()),
        paused: cfg.paused.clone(),
        url: Arc::new(Mutex::new(cfg.url.clone())),
        size: Arc::new(Mutex::new((cfg.width, cfg.height))),
        transparent: false,
    };
    let Some((browser, closed)) = create(&page_spec, on_gpu, &cfg.url, cfg.fps) else {
        cef::shutdown();
        anyhow::bail!("CEF failed to create the browser");
    };
    let mut page = Live {
        spec: page_spec,
        browser,
        closed,
        on_gpu,
        fps: cfg.fps,
    };
    // The overlay browser: same size, transparent; a failure leaves the
    // page running without overlays.
    let mut overlay = cfg.overlay.as_ref().and_then(|o| {
        let spec = Spec {
            slot: o.slot.clone(),
            audio: o.audio.clone(),
            on_texture: o.on_texture.clone(),
            paused: o.paused.clone(),
            url: Arc::new(Mutex::new(o.url.clone())),
            size: Arc::new(Mutex::new((cfg.width, cfg.height))),
            transparent: true,
        };
        let gpu = on_gpu && spec.on_texture.is_some();
        match create(&spec, gpu, &o.url, o.fps) {
            Some((browser, closed)) => Some(Live {
                spec,
                browser,
                closed,
                on_gpu: gpu,
                fps: o.fps,
            }),
            None => {
                tracing::error!("CEF failed to create the overlay browser; no overlay layers");
                None
            }
        }
    });

    let (tx, rx) = mpsc::channel::<(Target, Cmd)>();
    let overlay_handle = overlay.as_ref().map(|o| {
        Arc::new(BrowserHandle {
            tx: tx.clone(),
            target: Target::Overlay,
            url: o.spec.url.clone(),
            paused: o.spec.paused.clone(),
            size: o.spec.size.clone(),
            overlay: None,
        })
    });
    ready(BrowserHandle {
        tx,
        target: Target::Page,
        url: page.spec.url.clone(),
        paused: page.spec.paused.clone(),
        size: page.spec.size.clone(),
        overlay: overlay_handle,
    });

    // Pump CEF from this thread; poll commands between pumps.
    let mut closing_since: Option<Instant> = None;
    loop {
        cef::do_message_loop_work();
        while let Ok((target, cmd)) = rx.try_recv() {
            match cmd {
                Cmd::RecreateCpu if closing_since.is_none() => {
                    page.recreate_cpu("page");
                    if let Some(o) = &mut overlay {
                        o.recreate_cpu("overlay");
                    }
                }
                Cmd::RecreateCpu => {}
                Cmd::Shutdown if closing_since.is_none() => {
                    for l in std::iter::once(&page).chain(overlay.as_ref()) {
                        if let Some(host) = l.host() {
                            host.close_browser(1);
                        }
                    }
                    closing_since = Some(Instant::now());
                }
                Cmd::Shutdown => {}
                cmd => match (target, &mut overlay) {
                    (Target::Page, _) => page.apply(cmd),
                    (Target::Overlay, Some(o)) => o.apply(cmd),
                    (Target::Overlay, None) => {}
                },
            }
        }
        if should_exit(
            page.closed.load(Ordering::SeqCst),
            closing_since,
            Instant::now(),
        ) {
            if !page.closed.load(Ordering::SeqCst) {
                tracing::warn!("browser did not close within 3 s; shutting down anyway");
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    // The page closed by itself: close the overlay too, and give it up to 1 s.
    if let Some(o) = &overlay {
        if !o.closed.load(Ordering::SeqCst) {
            if closing_since.is_none() {
                if let Some(host) = o.host() {
                    host.close_browser(1);
                }
            }
            let t = Instant::now();
            while !o.closed.load(Ordering::SeqCst) && t.elapsed() < Duration::from_secs(1) {
                cef::do_message_loop_work();
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
    drop(page);
    drop(overlay);
    // Let CEF release the closed browsers before shutting down.
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

/// Chromium switches without a value, added to every launch.
const FLAG_SWITCHES: &[&str] = &[
    "noerrdialogs",
    "hide-crash-restore-bubble",
    "disable-session-crashed-bubble",
    "use-mock-keychain",
    // The page is offscreen, so Chromium would treat its renderer as hidden:
    // lower its priority (Windows then gave it Idle priority and EcoQoS) and
    // throttle its timers. The camera shows it live, so neither.
    "disable-renderer-backgrounding",
    "disable-background-timer-throttling",
];

/// `adapter_luid`: value for `use-adapter-luid` (see [`luid_switch_value`]).
pub(crate) fn add_switches(cmd: &mut cef::CommandLine, adapter_luid: Option<&str>) {
    if let Some(v) = adapter_luid {
        cmd.append_switch_with_value(
            Some(&CefString::from("use-adapter-luid")),
            Some(&CefString::from(v)),
        );
    }
    for s in FLAG_SWITCHES {
        cmd.append_switch(Some(&CefString::from(*s)));
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
    fn renderers_are_never_backgrounded() {
        // Windows put an offscreen renderer at Idle priority with EcoQoS:
        // Chromium must not background it nor throttle its timers.
        for s in [
            "disable-renderer-backgrounding",
            "disable-background-timer-throttling",
        ] {
            assert!(FLAG_SWITCHES.contains(&s), "missing switch {s}");
        }
    }

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
