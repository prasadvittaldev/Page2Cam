//! The running app as seen by the admin page.
use std::sync::{Arc, OnceLock};

use anyhow::Context;

use p2c_admin::{
    AudioSettings, BackgroundDto, Controls, GpuInfo, ImageRow, InputEvent, KeyAction, MouseBtn,
    NavAction, Status, StreamRequest,
};
use p2c_audio::{MicCapture, MixSettings, Mixer};
use p2c_browser::{BrowserHandle, KeyInput, KeyKind, MouseButton};
use p2c_compositor::{Backdrop, OverlayStyle, PipLayout};
use p2c_core::{AudioRing, FrameSlot};
use p2c_segment::{EngineChoice, Segmenter};
use p2c_stream::{StreamConfig, StreamController, StreamState};
use parking_lot::{Mutex, RwLock};

const KEYRING_SERVICE: &str = "page2cam";
const KEYRING_USER: &str = "stream-key";

pub struct App {
    pub browser: OnceLock<BrowserHandle>,
    pub layout: Arc<RwLock<PipLayout>>,
    /// Backgrounds and edge quality, read by the ticker every tick (kept in
    /// step with `bg`). The fallback note is in `ticker_stats.compose_note()`.
    pub backdrop: Arc<RwLock<Backdrop>>,
    /// The operator's background choices (full frame, in box, pictures,
    /// the page to restore).
    pub bg: Mutex<crate::background::BgState>,
    /// Serializes background changes (and the Box style rule), so their
    /// navigations and saves happen in order. Never held by `status()`.
    pub apply_lock: Mutex<()>,
    /// `http://127.0.0.1:<port>/bg/<id>/` once the admin server is up.
    pub bg_base: OnceLock<String>,
    /// Where uploaded pictures are kept (`<data dir>/backgrounds/`).
    pub data_dir: Option<std::path::PathBuf>,
    /// YouTube thumbnails for the admin page.
    pub thumbs: crate::background::ThumbCache,
    /// Webcam crop, read by the capture thread for every frame.
    pub crop: p2c_core::SharedCrop,
    pub stream: StreamController,
    pub mixer: Mixer,
    pub mic_ring: AudioRing,
    pub mic: Mutex<Option<MicCapture>>,
    pub mic_status: Mutex<String>,
    pub webcam_slot: Option<FrameSlot>,
    /// Live: "reconnecting: <error>" while the webcam is being reopened.
    pub webcam_status: p2c_webcam::WebcamStatus,
    pub vcam_status: String,
    /// Problems the camera sink found while running (e.g. an old driver); wins over `vcam_status`.
    pub vcam_live: p2c_vcam::CameraStatus,
    /// ffmpeg raw input format matching the ticker's output (`"nv12"` or `"bgra"`).
    pub stream_pix_fmt: &'static str,
    pub stream_url: Mutex<String>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_kbps: u32,
    pub fallback_url: Mutex<String>,
    pub log_file: String,
    /// Cut-out thread; None without a webcam.
    pub segmenter: Option<Segmenter>,
    /// Where operator choices (style, engine) are saved.
    pub settings_path: Option<std::path::PathBuf>,
    /// Mic delay currently applied, in ms, and recent cut-out latencies.
    pub mic_delay_ms: Mutex<u32>,
    pub latencies: Mutex<std::collections::VecDeque<u32>>,
    /// Output pipeline latency in ms (from the ticker; 0 on the CPU path).
    pub pipeline_ms: Arc<std::sync::atomic::AtomicU32>,
    pub updater: Arc<crate::update::Updater>,
    /// What makes the video: "GPU: <adapter>", "CPU (fallback: <reason>)",
    /// "CPU (compatibility)" (changes at run time on a fallback).
    pub video: Arc<Mutex<String>>,
    /// Encoder for the next stream: "probing…", "NVENC", "Quick Sync", "AMF" or "x264".
    pub encoder: Arc<Mutex<String>>,
    /// Saved `gpu` setting (normalized); applies at the next start.
    pub gpu_saved: Mutex<String>,
    /// The `gpu` setting this run started with (incl. `--gpu-override`).
    pub gpu_running: String,
    /// Started with `--gpu-override` (Chromium closed unexpectedly on the GPU).
    pub gpu_override: bool,
    /// The adapter DirectML runs the cut-out on (`None`: no hardware GPU).
    pub cutout_gpu: Option<String>,
    /// Why the browser loop ended (read by `main` after it returns).
    pub exit: Arc<crate::restart::ExitFlags>,
    /// Ends the wait in `--test-pattern` mode (no browser to shut down).
    /// Also held while the browser is set and the shutdown flag read in
    /// `main`, and while a shutdown is requested (see [`App::request_shutdown`]),
    /// so a request before the browser exists is never lost.
    pub quit_main: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    /// Output tick/skip totals and first-output flag (health).
    pub ticker_stats: Arc<p2c_compositor::TickerStats>,
    /// Recent samples for the health rates.
    pub health_window: Mutex<crate::health::HealthWindow>,
    /// The page frame rate and render size, lowered while the output skips ticks.
    pub page_fps: Mutex<crate::page_fps::PageQuality>,
    /// The screen background's capture.
    pub screen: crate::screen::ScreenShare,
    /// Chromium's paints are dropped while set (the screen fills the page);
    /// shared with the browser (`BrowserConfig.paused`).
    pub page_paused: Arc<std::sync::atomic::AtomicBool>,
    /// Latest output frame (shared with the preview sink); snapshots encode from it on demand.
    pub output_frame: FrameSlot,
    /// Overlay layers (titles, tickers, HTML…) and the overlay browser's on/off.
    pub overlay: crate::overlay::OverlayLayers,
}

/// `ev` (page pixels at the configured `from` size, as the admin maps them)
/// in the pixels of the page as it renders now (`to`: lowered by the quality
/// throttle). Wheel distances scale too, so a scroll moves the same share
/// of the page.
pub fn scale_input(ev: InputEvent, from: (u32, u32), to: (u32, u32)) -> InputEvent {
    if from == to || from.0 == 0 || from.1 == 0 {
        return ev;
    }
    let sx = |v: i32| (v as i64 * to.0 as i64 / from.0 as i64) as i32;
    let sy = |v: i32| (v as i64 * to.1 as i64 / from.1 as i64) as i32;
    match ev {
        InputEvent::Move { x, y, mods } => InputEvent::Move {
            x: sx(x),
            y: sy(y),
            mods,
        },
        InputEvent::Button {
            x,
            y,
            button,
            down,
            clicks,
            mods,
        } => InputEvent::Button {
            x: sx(x),
            y: sy(y),
            button,
            down,
            clicks,
            mods,
        },
        InputEvent::Wheel { x, y, dx, dy, mods } => InputEvent::Wheel {
            x: sx(x),
            y: sy(y),
            dx: sx(dx),
            dy: sy(dy),
            mods,
        },
        k @ InputEvent::Key { .. } => k,
    }
}

/// Lock `m` for the status/health readers. Debug builds panic if it stays
/// held for 2 s (a re-entrant lock deadlocks otherwise, silently); release
/// builds just lock.
fn lock<'a, T>(m: &'a Mutex<T>, what: &str) -> parking_lot::MutexGuard<'a, T> {
    #[cfg(debug_assertions)]
    {
        m.try_lock_for(std::time::Duration::from_secs(2))
            .unwrap_or_else(|| panic!("`{what}` lock still held after 2 s (re-entrant lock?)"))
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = what;
        m.lock()
    }
}

/// Longest mic delay applied for the cut-out (ms).
const MAX_MIC_DELAY_MS: u32 = 250;

/// Mic delay for these recent cut-out latencies: their median (so one slow
/// frame doesn't cause a jump), capped.
pub fn smoothed_delay(samples: &[u32]) -> u32 {
    if samples.is_empty() {
        return 0;
    }
    let mut v = samples.to_vec();
    v.sort_unstable();
    v[v.len() / 2].min(MAX_MIC_DELAY_MS)
}

/// Mic delay: the cut-out's median latency plus the output pipeline's (the
/// GPU readback ring holds frames back), capped.
pub fn mic_delay(inference: &[u32], pipeline_ms: u32) -> u32 {
    (smoothed_delay(inference) + pipeline_ms).min(MAX_MIC_DELAY_MS)
}

impl App {
    fn with_browser(&self, f: impl FnOnce(&BrowserHandle)) {
        if let Some(b) = self.browser.get() {
            f(b);
        }
    }

    fn saved_key() -> Option<String> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .ok()?
            .get_password()
            .ok()
    }

    fn save_key(key: &str) {
        match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).and_then(|e| e.set_password(key)) {
            Ok(()) => tracing::info!("stream key saved in the system keyring"),
            Err(e) => tracing::warn!(error = %e, "could not save the stream key"),
        }
    }

    fn update_mic(&self, want: bool) {
        let mut mic = self.mic.lock();
        if want && mic.is_none() {
            match MicCapture::start(self.mic_ring.clone()) {
                Ok(m) => {
                    tracing::info!(mic = %m.name, "microphone on");
                    *self.mic_status.lock() = format!("ok: {}", m.name);
                    *mic = Some(m);
                }
                Err(e) => {
                    tracing::warn!("microphone unavailable: {e:#}");
                    *self.mic_status.lock() = format!("error: {e:#}");
                }
            }
        } else if !want {
            if let Some(m) = mic.take() {
                m.stop();
                self.mic_ring.clear();
            }
            *self.mic_status.lock() = "off".into();
        }
    }
}

impl App {
    fn save_choice(&self, key: &str, value: &str) {
        let Some(p) = &self.settings_path else { return };
        match crate::config::save_choice(p, key, value) {
            Ok(()) => tracing::info!(key, value, file = %p.display(), "setting saved"),
            Err(e) => tracing::warn!("could not save {key}: {e:#}"),
        }
    }

    /// The cut-out line for the admin status bar.
    fn cutout_status(&self) -> String {
        let Some(seg) = &self.segmenter else {
            return "no webcam".into();
        };
        if !self.mask_in_use(false) {
            return "off (Box style)".into();
        }
        let st = seg.status();
        let running = match (st.active, st.device) {
            (Some(a), Some(d)) => format!("{a} · {d} · {:.0} masks/s", st.masks_per_sec),
            _ => String::new(),
        };
        match (running.is_empty(), st.note.is_empty()) {
            (true, true) => "starting…".into(),
            (true, false) => format!("error: {}", st.note),
            (false, true) => running,
            (false, false) => format!("{running}. {}", st.note),
        }
    }

    /// Where the cut-out runs while it is in use (see [`cutout_device_text`]).
    fn cutout_device(&self) -> String {
        let Some(seg) = &self.segmenter else {
            return String::new();
        };
        if !self.mask_in_use(false) {
            return String::new();
        }
        let st = seg.status();
        cutout_device_text(self.cutout_gpu.as_deref(), st.active.and(st.device))
    }

    /// Delay the mic by the cut-out's processing time plus the output pipeline's so lips match the
    /// voice on the stream: median of the last ~2 s of latencies, capped.
    /// Changes under 20 ms are ignored to avoid churn.
    pub fn update_mic_delay(&self) {
        let latest = match &self.segmenter {
            Some(seg) if self.mask_in_use(false) => {
                let st = seg.status();
                st.active.map(|_| st.latency_ms)
            }
            _ => None,
        };
        let want = {
            let mut l = self.latencies.lock();
            match latest {
                Some(ms) => {
                    l.push_back(ms);
                    if l.len() > 8 {
                        l.pop_front();
                    }
                }
                None => l.clear(),
            }
            mic_delay(
                l.make_contiguous(),
                self.pipeline_ms.load(std::sync::atomic::Ordering::Relaxed),
            )
        };
        let mut cur = self.mic_delay_ms.lock();
        if want.abs_diff(*cur) > 20 || (want == 0 && *cur != 0) {
            self.mixer.set_mic_delay_ms(want);
            tracing::debug!(ms = want, "mic delay");
            *cur = want;
        }
    }

    /// Record the running totals for the health rates (every ~1 s).
    pub fn sample_health(&self) {
        let s = crate::health::Sample {
            at: std::time::Instant::now(),
            ticks: self.ticker_stats.ticks(),
            skipped: self.ticker_stats.skipped(),
            webcam_frames: self.webcam_slot.as_ref().map_or(0, |s| s.seq()),
        };
        self.health_window.lock().push(s);
    }

    /// Make the page cheaper while the output skips ticks (a page that takes
    /// the whole GPU starves the compositor): lower frame rate, then lower
    /// render size; back up once it keeps up. The overlay browser follows the
    /// frame rate only. Call after [`App::sample_health`].
    pub fn govern_page_fps(&self) {
        if !self.ticker_stats.output_ready() {
            return;
        }
        let skipped = lock(&self.health_window, "health_window").skipped_pct();
        let changed = lock(&self.page_fps, "page_fps").update(std::time::Instant::now(), skipped);
        if let Some(level) = changed {
            let (w, h) = level.res.size(self.width, self.height);
            tracing::info!(
                fps = level.fps,
                size = %format_args!("{w}x{h}"),
                skipped_pct = skipped,
                "page quality changed (output skipping ticks)"
            );
            self.with_browser(|b| {
                b.set_frame_rate(level.fps);
                b.set_size(w, h);
            });
            self.overlay.set_fps(level.fps);
        }
    }

    /// The health block of the status (rates over the last ~10 s).
    pub fn health(&self) -> p2c_admin::Health {
        use crate::health::{cutout_auto_fallback, video_fallback, webcam_state};
        let l = *self.layout.read();
        let in_use = l.visible && self.backdrop.read().needs_mask(&l);
        let auto_fast = self.segmenter.as_ref().is_some_and(|s| {
            let st = s.status();
            cutout_auto_fallback(in_use, st.requested, st.active)
        });
        // One lock at a time (each released before the next is taken).
        let (skipped_pct, fps) = {
            let w = lock(&self.health_window, "health_window");
            (w.skipped_pct(), w.webcam_fps())
        };
        let video = lock(&self.video, "video").clone();
        let video_fallback = video_fallback(&video);
        p2c_admin::Health {
            output_ready: self.ticker_stats.output_ready(),
            output_skipped_pct: skipped_pct,
            webcam_fps: if self.webcam_slot.is_some() { fps } else { 0.0 },
            webcam_state: webcam_state(Some(&self.webcam_status.get())),
            cutout_auto_fallback: auto_fast,
            video_fallback,
            camera_status: self.camera_status(),
        }
    }

    fn camera_status(&self) -> String {
        self.vcam_live
            .status()
            .unwrap_or_else(|| self.vcam_status.clone())
    }

    /// What ends the main loop: the browser, or (test pattern) the wait.
    /// Call with `quit_main` locked (passed in).
    fn closer(
        &self,
        quit_main: &Option<std::sync::mpsc::Sender<()>>,
    ) -> Option<Box<dyn FnOnce() + Send>> {
        match (self.browser.get().cloned(), quit_main.clone()) {
            (Some(b), _) => Some(Box::new(move || b.shutdown())),
            (None, Some(q)) => Some(Box::new(move || {
                let _ = q.send(());
            })),
            (None, None) => None,
        }
    }

    /// The normal shutdown (as Ctrl-C): set the flag and close the main
    /// loop after `delay`. Before the main loop exists only the flag is set;
    /// `main` checks it once the loop can be closed.
    pub fn request_shutdown(&self, delay: std::time::Duration) {
        let close = {
            let q = self.quit_main.lock();
            self.exit
                .user_shutdown
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.closer(&q)
        };
        if let Some(close) = close {
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                close();
            });
        }
    }
}

/// Note shown when a full-frame background switched the style to a cut-out.
const NOTE_TO_CUTOUT: &str =
    "Style set to full-height cut-out: a full-frame background needs the person cut out";
/// Note shown when choosing Box put the full frame back to the page.
/// `last_page_url` is written at most this often while the page moves.
const PAGE_URL_SAVE_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

const NOTE_TO_PAGE: &str = "Full-frame background set back to Page: the Box style shows the page";

/// One change of the background choices (see [`App::apply_background`]).
struct BgChange {
    full: crate::background::FullBg,
    in_box: crate::background::BoxBg,
    refined: bool,
    /// The operator asked for this page (admin navigation).
    page_url: Option<String>,
    /// Reload the generated page even if the choice is the same (new picture).
    reload: bool,
    /// Extra settings to write with the change.
    extra: Vec<(&'static str, toml_edit::Value)>,
}

impl App {
    /// Load `url` in Chromium (or remember it until the browser exists).
    fn load_url(&self, url: &str) {
        let from = self.shown_url();
        {
            let mut s = self.bg.lock();
            s.history_moved = false;
            s.load_from = from;
        }
        *self.fallback_url.lock() = url.to_string();
        self.with_browser(|b| b.navigate(url));
    }

    /// Back/Forward are ignored while a generated background shows (the
    /// history behind it is the user's page or other backgrounds).
    pub fn nav_allowed(&self, a: NavAction) -> bool {
        a == NavAction::Reload || !self.bg.lock().full.is_generated()
    }

    /// If Chromium shows one of our background pages while none is chosen
    /// (Back/Forward, or the page's own `history.back()`), load the user's
    /// page instead. Acts only once history moved since our last
    /// [`App::load_url`] (see `BgState::history_moved`): while our own load
    /// is still pending Chromium reports the old URL, so a slow load is
    /// never reloaded. Called every ~250 ms.
    pub fn keep_page_off_background(&self) {
        let _apply = self.apply_lock.lock();
        let shown = self.shown_url();
        let url = {
            let mut s = self.bg.lock();
            if shown != s.load_from {
                s.history_moved = true;
            }
            let on_bg = !crate::background::is_user_page_url(&shown) && shown.contains("/bg/");
            (on_bg && s.history_moved && !s.full.is_generated()).then(|| s.last_page_url.clone())
        };
        if let Some(u) = url.filter(|u| !u.is_empty()) {
            tracing::info!(url = %u, "history reached a background page: back to the user's page");
            self.load_url(&u);
            return;
        }
        // The page's own navigations and Back/Forward.
        self.remember_page_url(&shown, std::time::Instant::now());
    }

    /// The user's page changed while Chromium shows it (no generated
    /// background up): remember `url` as `last_page_url` at once and save
    /// it, at most once per [`PAGE_URL_SAVE_EVERY`] (a later call writes
    /// what is pending). A `/bg/` or non-page URL is never remembered.
    /// Callers hold `apply_lock`.
    pub(crate) fn remember_page_url(&self, url: &str, now: std::time::Instant) {
        let save = {
            let mut s = self.bg.lock();
            if !s.full.is_generated()
                && crate::background::is_user_page_url(url)
                && url != s.last_page_url
            {
                s.last_page_url = url.to_string();
                s.page_url_unsaved = true;
            }
            let due = s
                .page_url_saved_at
                .is_none_or(|t| now.saturating_duration_since(t) >= PAGE_URL_SAVE_EVERY);
            (s.page_url_unsaved && due).then(|| {
                s.page_url_unsaved = false;
                s.page_url_saved_at = Some(now);
                s.last_page_url.clone()
            })
        };
        if let Some(u) = save {
            self.save_values(&[("last_page_url", u.as_str().into())]);
        }
    }

    /// The URL Chromium shows.
    fn shown_url(&self) -> String {
        self.browser
            .get()
            .map(|b| b.current_url())
            .unwrap_or_else(|| lock(&self.fallback_url, "fallback_url").clone())
    }

    fn save_values(&self, values: &[(&str, toml_edit::Value)]) {
        let Some(p) = &self.settings_path else { return };
        if values.is_empty() {
            return;
        }
        match crate::config::save_values(p, values) {
            Ok(()) => tracing::info!(file = %p.display(), "background settings saved"),
            Err(e) => tracing::warn!("could not save the background settings: {e:#}"),
        }
    }

    /// Copy the choices to the compositor and run the cut-out when they need it.
    fn sync_backdrop(&self) {
        let b = self.bg.lock().backdrop();
        *self.backdrop.write() = b;
        self.update_segmenter();
        self.sync_screen();
    }

    /// Capture the chosen monitor while the screen background shows, with
    /// Chromium's paints paused (paused before the first captured frame,
    /// resumed after the capture stopped, then repainted).
    fn sync_screen(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        let want = self.bg.lock().full.screen().map(str::to_string);
        match want {
            Some(id) => {
                self.page_paused.store(true, SeqCst);
                self.screen.sync(Some(&id));
            }
            None => {
                self.screen.sync(None);
                if self.page_paused.swap(false, SeqCst) {
                    self.with_browser(|b| b.repaint());
                }
            }
        }
    }

    /// The cut-out runs while the overlay or a backdrop needs the mask.
    fn update_segmenter(&self) {
        if let Some(seg) = &self.segmenter {
            seg.set_enabled(self.mask_in_use(true));
        }
    }

    /// Whether drawing needs the person mask (`visible`: only if the
    /// overlay is shown).
    fn mask_in_use(&self, visible: bool) -> bool {
        let l = *self.layout.read();
        (!visible || l.visible) && self.backdrop.read().needs_mask(&l)
    }

    /// The generated page's URL (`None` before the admin server is up).
    fn bg_page_url(&self, version: u64) -> Option<String> {
        self.bg_base
            .get()
            .map(|b| format!("{b}current.html?v={version}"))
    }

    /// The admin server is up: generated pages live under `base`. Returns
    /// the URL Chromium should start on when a saved background is a
    /// generated page (and loads it if the browser already exists).
    pub fn start_background(&self, base: String) -> Option<String> {
        let _held = self.apply_lock.lock();
        let _ = self.bg_base.set(base);
        let url = {
            let s = self.bg.lock();
            s.full
                .is_generated()
                .then(|| self.bg_page_url(s.version))
                .flatten()
        };
        if let Some(u) = &url {
            self.load_url(u);
        }
        self.sync_backdrop();
        url
    }

    /// Apply a background change: remember the user's page, navigate
    /// Chromium, apply the interplay rule (a full-frame background needs a
    /// cut-out style), update the compositor and save everything.
    /// Callers hold `apply_lock` (passed in) for the whole change.
    fn apply_background(&self, _held: &parking_lot::MutexGuard<'_, ()>, c: BgChange) {
        use crate::background::{is_user_page_url, FullBg};
        let shown = self.shown_url();
        let style = self.layout.read().style;
        let to_cutout = c.full != FullBg::Page && style == OverlayStyle::Box;
        let mut saves = c.extra;
        let nav = {
            let mut s = self.bg.lock();
            let old_last = s.last_page_url.clone();
            // Chromium shows the user's page unless a generated one is up
            // (and a `/bg/` URL is never the user's page).
            if !s.full.is_generated() && is_user_page_url(&shown) {
                s.last_page_url = shown.clone();
            }
            if let Some(u) = c.page_url.filter(|u| is_user_page_url(u)) {
                s.last_page_url = u;
            }
            // Chromium is on one of our pages (e.g. its navigation away
            // from it hasn't finished): never leave it there for Page.
            let shown_is_bg = !is_user_page_url(&shown) && shown.contains("/bg/");
            let was_generated = s.full.is_generated();
            let full_changed = s.full != c.full;
            s.full = c.full;
            s.in_box = c.in_box;
            s.refined = c.refined;
            match &s.full {
                FullBg::Colour(col) => s.full_colour = col.clone(),
                FullBg::YouTube(id) => s.youtube = id.clone(),
                FullBg::Screen(id) => s.screen = id.clone(),
                _ => {}
            }
            if let crate::background::BoxBg::Colour(col) = &s.in_box {
                s.box_colour = col.clone();
            }
            if to_cutout {
                s.note = Some((NOTE_TO_CUTOUT.into(), std::time::Instant::now()));
            }
            saves.push(("background", s.full.as_str().into()));
            saves.push(("box_background", s.in_box.as_str().into()));
            saves.push(("refined_edges", s.refined.into()));
            match &s.full {
                FullBg::Colour(col) => saves.push(("background_colour", col.as_str().into())),
                FullBg::YouTube(id) => saves.push(("background_youtube", id.as_str().into())),
                FullBg::Screen(id) => saves.push(("background_screen", id.as_str().into())),
                _ => {}
            }
            if let crate::background::BoxBg::Colour(col) = &s.in_box {
                saves.push(("box_background_colour", col.as_str().into()));
            }
            if s.last_page_url != old_last || s.page_url_unsaved {
                saves.push(("last_page_url", s.last_page_url.as_str().into()));
                s.page_url_unsaved = false;
            }
            if s.full.is_generated() {
                if full_changed || c.reload {
                    s.version += 1;
                    self.bg_page_url(s.version)
                } else {
                    None
                }
            } else if was_generated || shown_is_bg {
                Some(if s.last_page_url.is_empty() {
                    "about:blank".to_string()
                } else {
                    s.last_page_url.clone()
                })
            } else {
                None
            }
        };
        if to_cutout {
            let mut l = self.layout.write();
            l.style = OverlayStyle::CutoutFull;
            drop(l);
            tracing::info!("full-frame background chosen in Box style: style set to cutout-full");
            saves.push(("overlay_style", OverlayStyle::CutoutFull.as_str().into()));
        }
        if let Some(u) = nav {
            tracing::info!(url = %u, "background page");
            self.load_url(&u);
        }
        self.sync_backdrop();
        self.save_values(&saves);
    }

    /// The current choices with one part changed.
    fn change(&self) -> BgChange {
        let s = self.bg.lock();
        BgChange {
            full: s.full.clone(),
            in_box: s.in_box.clone(),
            refined: s.refined,
            page_url: None,
            reload: false,
            extra: Vec::new(),
        }
    }

    /// The text under the Background section: a recent automatic change,
    /// else why the output doesn't show the choice.
    fn background_note(&self) -> String {
        if let Some(e) = self.screen.error() {
            return e;
        }
        if let Some(n) = self.bg.lock().recent_note() {
            return n.to_string();
        }
        self.ticker_stats.compose_note().as_str().to_string()
    }

    /// Parse the admin page's choices (nothing is changed on an error).
    fn parse_background(&self, d: &BackgroundDto) -> anyhow::Result<BgChange> {
        use crate::background::{normalize_colour, youtube_id, BoxBg, FullBg};
        let s = self.bg.lock();
        let colour = |given: &Option<String>, last: &str| {
            normalize_colour(given.as_deref().unwrap_or(last)).map_err(anyhow::Error::msg)
        };
        let full = match d.full.as_str() {
            "page" => FullBg::Page,
            "image" => {
                anyhow::ensure!(s.full_image.is_some(), "upload a full-frame image first");
                FullBg::Image
            }
            "colour" => FullBg::Colour(colour(&d.full_colour, &s.full_colour)?),
            "youtube" => FullBg::YouTube(
                youtube_id(d.full_youtube.as_deref().unwrap_or(&s.youtube))
                    .map_err(anyhow::Error::msg)?,
            ),
            "screen" => FullBg::Screen(
                self.screen
                    .resolve(d.full_screen.as_deref().unwrap_or(&s.screen))?,
            ),
            "blurred-room" => FullBg::BlurredRoom,
            other => anyhow::bail!(
                "unknown full-frame background {other:?} (page, image, colour, youtube, screen or blurred-room)"
            ),
        };
        let in_box = match d.in_box.as_str() {
            "room" => BoxBg::Room,
            "blur" => BoxBg::Blur,
            "image" => {
                anyhow::ensure!(s.box_image.is_some(), "upload an in-box image first");
                BoxBg::Image
            }
            "colour" => BoxBg::Colour(colour(&d.box_colour, &s.box_colour)?),
            other => {
                anyhow::bail!("unknown in-box background {other:?} (room, blur, image or colour)")
            }
        };
        Ok(BgChange {
            full,
            in_box,
            refined: d.refined_edges,
            page_url: None,
            reload: false,
            extra: Vec::new(),
        })
    }
}

impl p2c_admin::Overlays for App {
    fn layers(&self) -> Vec<serde_json::Value> {
        self.overlay.layers()
    }
    fn add_layer(&self, spec: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        self.overlay.add(spec)
    }
    fn update_layer(
        &self,
        id: &str,
        patch: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        self.overlay.update(id, patch)
    }
    fn remove_layer(&self, id: &str) -> anyhow::Result<()> {
        self.overlay.remove(id)
    }
    fn clear_layers(&self) -> usize {
        self.overlay.clear()
    }
    fn reorder_layers(&self, ids: Vec<String>) -> anyhow::Result<()> {
        self.overlay.reorder(ids)
    }
    fn set_layer_asset(&self, id: &str, bytes: Vec<u8>) -> anyhow::Result<serde_json::Value> {
        self.overlay.set_asset(id, &bytes)
    }
    fn overlay_state(&self, since: Option<u64>) -> Option<serde_json::Value> {
        self.overlay.state(since)
    }
    fn overlay_layer_page(&self, id: &str) -> Option<String> {
        self.overlay.layer_page(id)
    }
    fn overlay_asset(&self, name: &str) -> Option<(Vec<u8>, &'static str)> {
        self.overlay.asset(name)
    }
    fn templates(&self, category: Option<String>) -> serde_json::Value {
        self.overlay.templates(category.as_deref())
    }
    fn template(&self, id: &str) -> Option<serde_json::Value> {
        self.overlay.template(id)
    }
    fn save_template(&self, def: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        self.overlay.save_template(def)
    }
    fn delete_template(&self, id: &str) -> anyhow::Result<()> {
        self.overlay.delete_template(id)
    }
    fn scenes(&self) -> serde_json::Value {
        self.overlay.scenes()
    }
    fn save_scene(
        &self,
        name: &str,
        background: bool,
        webcam: bool,
    ) -> anyhow::Result<serde_json::Value> {
        let bg = background
            .then(|| serde_json::to_value(Controls::background(self)).ok())
            .flatten();
        let cam = webcam
            .then(|| serde_json::to_value(p2c_admin::PipDto::from(Controls::pip(self))).ok())
            .flatten();
        self.overlay.save_scene(name, bg, cam)
    }
    /// The layers come back first; then the saved background and webcam
    /// (a failure there is reported, the layers stay).
    fn load_scene(&self, id: &str, replace: bool) -> anyhow::Result<serde_json::Value> {
        let (scene, r) = self.overlay.load_scene(id, replace)?;
        let mut notes: Vec<String> = Vec::new();
        let mut full = None;
        if let Some(bg) = scene.background {
            match serde_json::from_value::<BackgroundDto>(bg)
                .map_err(anyhow::Error::from)
                .and_then(|b| {
                    let f = b.full.clone();
                    Controls::set_background(self, b).map(|()| f)
                }) {
                Ok(f) => {
                    notes.push("background restored".into());
                    full = Some(f);
                }
                Err(e) => notes.push(format!("background not restored: {e:#}")),
            }
        }
        if let Some(cam) = scene.webcam {
            match serde_json::from_value::<p2c_admin::PipDto>(cam)
                .map_err(anyhow::Error::from)
                .and_then(PipLayout::try_from)
            {
                Ok(l) => {
                    Controls::set_pip(self, l);
                    notes.push("webcam overlay restored".into());
                }
                Err(e) => notes.push(format!("webcam overlay not restored: {e:#}")),
            }
        }
        tracing::info!(
            scene = id,
            added = r.added.len(),
            skipped = r.skipped.len(),
            "scene loaded"
        );
        Ok(serde_json::json!({
            "scene": scene.id, "added": r.added, "skipped": r.skipped, "notes": notes,
            "background_full": full,
        }))
    }
    fn delete_scene(&self, id: &str) -> anyhow::Result<()> {
        self.overlay.delete_scene(id)
    }
}

impl Controls for App {
    fn navigate(&self, url: &str) {
        tracing::info!(url, "navigate");
        let held = self.apply_lock.lock();
        if self.bg.lock().full.is_generated() {
            // The operator wants a page: the full frame goes back to Page.
            let mut c = self.change();
            c.full = crate::background::FullBg::Page;
            c.page_url = Some(url.to_string());
            self.apply_background(&held, c);
            if !crate::background::is_user_page_url(url) {
                self.load_url(url);
            }
            return;
        }
        self.load_url(url);
        self.remember_page_url(url, std::time::Instant::now());
    }

    fn nav(&self, a: NavAction) {
        // In order with background changes (and the watchdog).
        let _apply = self.apply_lock.lock();
        if !self.nav_allowed(a) {
            tracing::info!(action = ?a, "ignored while a background shows");
            return;
        }
        if a != NavAction::Reload {
            self.bg.lock().history_moved = true;
        }
        self.with_browser(|b| match a {
            NavAction::Back => b.back(),
            NavAction::Forward => b.forward(),
            NavAction::Reload => b.reload(),
        });
    }

    fn input(&self, ev: InputEvent) {
        self.with_browser(
            |b| match scale_input(ev, (self.width, self.height), b.size()) {
                InputEvent::Move { x, y, mods } => b.mouse_move(x, y, mods),
                InputEvent::Button {
                    x,
                    y,
                    button,
                    down,
                    clicks,
                    mods,
                } => {
                    let button = match button {
                        MouseBtn::Left => MouseButton::Left,
                        MouseBtn::Middle => MouseButton::Middle,
                        MouseBtn::Right => MouseButton::Right,
                    };
                    b.mouse_button(x, y, button, down, clicks, mods)
                }
                InputEvent::Wheel { x, y, dx, dy, mods } => b.wheel(x, y, dx, dy, mods),
                InputEvent::Key {
                    action,
                    code,
                    ch,
                    mods,
                } => b.key(KeyInput {
                    kind: match action {
                        KeyAction::Down => KeyKind::Down,
                        KeyAction::Up => KeyKind::Up,
                        KeyAction::Char => KeyKind::Char,
                    },
                    windows_key_code: code,
                    character: ch,
                    modifiers: mods,
                }),
            },
        );
    }

    fn pip(&self) -> PipLayout {
        *self.layout.read()
    }

    fn set_pip(&self, l: PipLayout) {
        // The style write and the Box rule as one step with background
        // changes (no Box with a full-frame background left behind).
        let held = self.apply_lock.lock();
        let old = std::mem::replace(&mut *self.layout.write(), l);
        if old.style != l.style {
            tracing::info!(style = l.style.as_str(), "overlay style");
            self.save_choice("overlay_style", l.style.as_str());
        }
        // Box chosen while a full-frame background shows: back to the page.
        if l.style == OverlayStyle::Box && self.bg.lock().full != crate::background::FullBg::Page {
            tracing::info!("Box style chosen: full-frame background set back to Page");
            let mut c = self.change();
            c.full = crate::background::FullBg::Page;
            self.apply_background(&held, c);
            self.bg.lock().note = Some((NOTE_TO_PAGE.into(), std::time::Instant::now()));
        }
        self.update_segmenter();
    }

    fn start_stream(&self, req: StreamRequest) -> anyhow::Result<()> {
        let key = if req.key.trim().is_empty() {
            Self::saved_key().ok_or_else(|| anyhow::anyhow!("enter your stream key"))?
        } else {
            req.key.trim().to_string()
        };
        if req.remember && !req.key.trim().is_empty() {
            Self::save_key(&key);
        }
        let ffmpeg = p2c_stream::find_ffmpeg().ok_or_else(|| {
            anyhow::anyhow!(
                "ffmpeg was not found. Put ffmpeg{} next to page2cam or on PATH",
                std::env::consts::EXE_SUFFIX
            )
        })?;
        *self.stream_url.lock() = req.url.trim().to_string();
        let cfg = StreamConfig {
            url: req.url.trim().to_string(),
            key,
            width: self.width,
            height: self.height,
            fps: self.fps,
            video_kbps: self.video_kbps,
            pix_fmt: self.stream_pix_fmt,
            encoder: p2c_stream::Encoder::X264, // replaced by the controller (set_encoder)
        };
        self.stream.start(cfg, &ffmpeg)
    }

    fn stop_stream(&self) {
        tracing::info!("stream stopped by operator");
        self.stream.stop();
    }

    fn audio(&self) -> AudioSettings {
        let s = self.mixer.settings();
        AudioSettings {
            page_on: s.page_on,
            page_gain: s.page_gain,
            mic_on: s.mic_on,
            mic_gain: s.mic_gain,
        }
    }

    fn set_audio(&self, a: AudioSettings) {
        self.mixer.set(MixSettings {
            page_on: a.page_on,
            page_gain: a.page_gain.clamp(0.0, 2.0),
            mic_on: a.mic_on,
            mic_gain: a.mic_gain.clamp(0.0, 2.0),
        });
        self.update_mic(a.mic_on);
    }

    fn status(&self) -> Status {
        let url = self.shown_url();
        let webcam_aspect = self
            .webcam_slot
            .as_ref()
            .and_then(|s| s.latest())
            .map(|f| f.height as f32 / f.width.max(1) as f32)
            .unwrap_or(9.0 / 16.0);
        let stream = match self.stream.state() {
            StreamState::Idle => "idle".to_string(),
            s => s.to_string(),
        };
        // Every locked value is read into its own local (each guard dropped
        // at the end of its `let`): a guard alive inside the literal below
        // would last the whole statement and deadlock any call in it that
        // takes the same lock again (it happened with `video`/`health`).
        let gpu_saved = lock(&self.gpu_saved, "gpu_saved").clone();
        let stream_url = lock(&self.stream_url, "stream_url").clone();
        let mic = lock(&self.mic_status, "mic_status").clone();
        let video = display_video(&lock(&self.video, "video").clone());
        let probed = lock(&self.encoder, "encoder").clone();
        let encoder = encoder_text(&probed, self.stream.encoder_fallback());
        let vcam = self.camera_status();
        let webcam = self.webcam_status.get();
        let cutout = self.cutout_status();
        let cutout_engine = self.cutout_engine();
        let cutout_gpu = self.cutout_device();
        let update = self.updater.info();
        let has_saved_key = Self::saved_key().is_some();
        let health = self.health();
        // While a generated background shows, the admin page keeps the
        // user's page URL (never a /bg/ URL).
        let (background, box_background, refined_edges, url) = {
            let s = lock(&self.bg, "bg");
            let url = if s.full.is_generated()
                || !crate::background::is_user_page_url(&url) && url.contains("/bg/")
            {
                s.last_page_url.clone()
            } else {
                url
            };
            (
                s.full.as_str().to_string(),
                s.in_box.as_str().to_string(),
                s.refined,
                url,
            )
        };
        let background_note = self.background_note();
        Status {
            url,
            width: self.width,
            height: self.height,
            fps: self.fps,
            vcam,
            webcam,
            webcam_aspect,
            stream,
            stream_url,
            has_saved_key,
            mic,
            log_file: self.log_file.clone(),
            cutout,
            cutout_engine,
            update,
            video,
            encoder,
            restart_needed: restart_needed(&gpu_saved, &self.gpu_running),
            gpu_override: self.gpu_override,
            cutout_gpu,
            gpu_choice: gpu_saved,
            gpu_running: self.gpu_running.clone(),
            health,
            agents_allowed: p2c_admin::TrayLink::global().and_then(|l| l.allowed()),
            overlay_layers: self.overlay.visible_count(),
            overlay_on: self.overlay.is_active(),
            background,
            box_background,
            refined_edges,
            background_note,
        }
    }

    fn install_update(&self) -> anyhow::Result<()> {
        self.updater.install()?;
        // The installer waits (up to ~15 s) for Page2Cam to close, then
        // replaces it and starts it again. Close shortly, after this request
        // has been answered.
        tracing::info!("closing for the update");
        self.exit
            .user_shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let browser = self.browser.get().cloned();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            match browser {
                Some(b) => b.shutdown(),
                None => std::process::exit(0),
            }
        });
        Ok(())
    }

    fn gpus(&self) -> Vec<GpuInfo> {
        gpu_list(&p2c_d3d::enumerate())
    }

    fn set_gpu_choice(&self, choice: &str) -> anyhow::Result<()> {
        let choice = check_gpu_choice(choice, &self.gpus())?;
        tracing::info!(gpu = %choice, running = %self.gpu_running, "GPU choice saved; applies after a restart");
        self.save_choice("gpu", &choice);
        *self.gpu_saved.lock() = choice;
        Ok(())
    }

    fn restart(&self) -> anyhow::Result<()> {
        tracing::info!("restart requested from the admin page");
        let close = self.closer(&self.quit_main.lock());
        begin_restart(&self.exit, close)
    }

    fn quit(&self) {
        tracing::info!("quit requested over the admin API");
        // Answer the request first, then close.
        self.request_shutdown(std::time::Duration::from_millis(200));
    }

    fn cutout_engine(&self) -> String {
        self.segmenter
            .as_ref()
            .map(|s| s.status().requested)
            .unwrap_or_default()
            .as_str()
            .into()
    }

    fn set_cutout_engine(&self, engine: &str) -> anyhow::Result<()> {
        let choice = EngineChoice::parse(engine)
            .ok_or_else(|| anyhow::anyhow!("unknown cut-out engine {engine:?}"))?;
        let seg = self
            .segmenter
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no webcam, so no cut-out"))?;
        tracing::info!(engine, "cut-out engine chosen");
        seg.set_choice(choice);
        self.save_choice("cutout_engine", engine);
        Ok(())
    }

    fn crop(&self) -> p2c_admin::CropDto {
        (*self.crop.read()).into()
    }

    fn set_crop(&self, c: p2c_admin::CropDto, save: bool) {
        change_crop(&self.crop, self.settings_path.as_deref(), c.into(), save);
    }

    fn snapshot(&self) -> Option<Vec<u8>> {
        let frame = self.output_frame.latest()?;
        match p2c_admin::encode_snapshot(&frame) {
            Ok(jpg) => Some(jpg),
            Err(e) => {
                tracing::warn!(error = %e, "snapshot encode failed");
                None
            }
        }
    }

    fn background(&self) -> BackgroundDto {
        let s = self.bg.lock();
        BackgroundDto {
            full: s.full.as_str().into(),
            full_colour: Some(s.full_colour.clone()),
            full_youtube: (!s.youtube.is_empty()).then(|| s.youtube.clone()),
            full_screen: Some(s.screen.clone()),
            in_box: s.in_box.as_str().into(),
            box_colour: Some(s.box_colour.clone()),
            refined_edges: s.refined,
            full_image: s.full_image.is_some(),
            box_image: s.box_image.is_some(),
        }
    }

    fn set_background(&self, b: BackgroundDto) -> anyhow::Result<()> {
        let held = self.apply_lock.lock();
        let change = self.parse_background(&b)?;
        tracing::info!(
            full = change.full.as_str(),
            in_box = change.in_box.as_str(),
            refined = change.refined,
            "background chosen"
        );
        self.apply_background(&held, change);
        Ok(())
    }

    fn set_background_image(&self, row: ImageRow, bytes: Vec<u8>) -> anyhow::Result<()> {
        use crate::background::{content_type, prepare_image, BoxBg, FullBg, StoredImage};
        let p = prepare_image(&bytes)?;
        tracing::info!(
            row = row.as_str(),
            w = p.width,
            h = p.height,
            "background image uploaded"
        );
        // After the (slow) decode: store and show as one step.
        let held = self.apply_lock.lock();
        let path = match &self.data_dir {
            Some(d) => {
                let dir = d.join("backgrounds");
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
                let path = dir.join(format!("{}.{}", row.as_str(), p.ext));
                std::fs::write(&path, &p.encoded)
                    .with_context(|| format!("writing {}", path.display()))?;
                // One picture per row: drop the other format's file.
                for ext in ["png", "jpg"].iter().filter(|e| **e != p.ext) {
                    let _ = std::fs::remove_file(dir.join(format!("{}.{ext}", row.as_str())));
                }
                Some(path)
            }
            None => {
                tracing::warn!("no data folder: the picture is kept until Page2Cam closes");
                None
            }
        };
        let img = StoredImage {
            bytes: Arc::new(p.encoded),
            content_type: content_type(p.ext),
            frame: Arc::new(p.frame),
        };
        let mut c = self.change();
        let key = match row {
            ImageRow::Full => {
                self.bg.lock().full_image = Some(img);
                c.full = FullBg::Image;
                c.reload = true;
                "background_image"
            }
            ImageRow::Box => {
                self.bg.lock().box_image = Some(img);
                c.in_box = BoxBg::Image;
                "box_background_image"
            }
        };
        if let Some(p) = &path {
            c.extra.push((key, p.display().to_string().into()));
        }
        self.apply_background(&held, c);
        Ok(())
    }

    fn background_page(&self) -> Option<String> {
        let s = self.bg.lock();
        s.full.is_generated().then(|| {
            crate::background::background_html(&s.full, Some(&format!("image?v={}", s.version)))
        })
    }

    fn background_image(&self, row: ImageRow) -> Option<(Arc<Vec<u8>>, &'static str)> {
        let s = self.bg.lock();
        let img = match row {
            ImageRow::Full => s.full_image.as_ref(),
            ImageRow::Box => s.box_image.as_ref(),
        }?;
        Some((img.bytes.clone(), img.content_type))
    }

    fn screens(&self) -> Result<Vec<p2c_admin::ScreenDto>, String> {
        let list = self.screen.list().map_err(|e| format!("{e:#}"))?;
        Ok(list
            .into_iter()
            .map(|s| p2c_admin::ScreenDto {
                id: s.id,
                index: s.index,
                name: s.name,
                width: s.width,
                height: s.height,
                primary: s.primary,
            })
            .collect())
    }

    fn youtube_thumb(&self, link: Option<String>) -> anyhow::Result<Arc<Vec<u8>>> {
        let id = match link {
            Some(l) => crate::background::youtube_id(&l).map_err(anyhow::Error::msg)?,
            None => {
                let id = self.bg.lock().youtube.clone();
                anyhow::ensure!(!id.is_empty(), "no YouTube video chosen");
                id
            }
        };
        self.thumbs.get(&id)
    }
}

/// Apply a crop (clamped) to the shared value the capture thread reads per
/// frame; with `save`, also write it to the settings file. Returns it as applied.
pub fn change_crop(
    shared: &p2c_core::SharedCrop,
    settings: Option<&std::path::Path>,
    crop: p2c_core::Crop,
    save: bool,
) -> p2c_core::Crop {
    let crop = crop.clamp();
    *shared.write() = crop;
    if save {
        tracing::info!(?crop, "webcam crop set");
        if let Some(p) = settings {
            match crate::config::save_crop(p, crop) {
                Ok(()) => tracing::info!(file = %p.display(), "webcam crop saved"),
                Err(e) => tracing::warn!("could not save the webcam crop: {e:#}"),
            }
        }
    }
    crop
}

/// "Restart now": set the restart flag and, shortly after (once the request
/// has been answered), call `close` to end the main loop; `main` then starts
/// the new process. Without a way to close yet (still starting up), refuse
/// without setting the flag, so a later Ctrl-C doesn't relaunch.
pub fn begin_restart(
    exit: &crate::restart::ExitFlags,
    close: Option<Box<dyn FnOnce() + Send>>,
) -> anyhow::Result<()> {
    // Already closing (quit, tray gone): that wins, no restart (exit 0, not 75).
    if exit.user_shutdown.load(std::sync::atomic::Ordering::SeqCst) {
        anyhow::bail!("Page2Cam is closing");
    }
    let Some(close) = close else {
        anyhow::bail!("Page2Cam is still starting; try again in a moment");
    };
    exit.restart
        .store(true, std::sync::atomic::Ordering::SeqCst);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        close();
    });
    Ok(())
}

/// The `gpu` setting in its normal form: "auto" | "cpu" | an adapter name.
pub fn normalize_gpu(s: &str) -> String {
    p2c_d3d::GpuSetting::parse(s).as_str().to_string()
}

/// R32: the saved `gpu` choice differs from the one this run started with.
pub fn restart_needed(saved: &str, running: &str) -> bool {
    normalize_gpu(saved) != normalize_gpu(running)
}

const NVIDIA: u32 = 0x10DE;
const AMD: u32 = 0x1002;

/// The picker's list (R33): "auto", each hardware adapter by name, "cpu".
/// "discrete": NVIDIA (no integrated GPUs on Windows PCs), or AMD when it is
/// Windows' high-performance pick among several; everything else "onboard".
pub fn gpu_list(adapters: &[p2c_d3d::AdapterInfo]) -> Vec<GpuInfo> {
    let info = |name: &str, kind: &str| GpuInfo {
        name: name.into(),
        kind: kind.into(),
    };
    let hw: Vec<_> = adapters.iter().filter(|a| !a.software).collect();
    let best = hw
        .iter()
        .min_by_key(|a| (a.perf_rank, a.index))
        .map(|a| a.luid);
    let mut out = vec![info("auto", "auto")];
    for a in &hw {
        if out.iter().any(|g| g.name == a.name) {
            continue; // the setting stores a name: one entry per name
        }
        let discrete =
            a.vendor_id == NVIDIA || (a.vendor_id == AMD && hw.len() > 1 && best == Some(a.luid));
        out.push(info(&a.name, if discrete { "discrete" } else { "onboard" }));
    }
    out.push(info("cpu", "cpu"));
    out
}

/// `choice` normalized, if the picker offers it.
pub fn check_gpu_choice(choice: &str, list: &[GpuInfo]) -> anyhow::Result<String> {
    let c = normalize_gpu(choice);
    anyhow::ensure!(
        list.iter().any(|g| g.name == c),
        "unknown GPU choice {choice:?} (not \"auto\", \"cpu\" or a GPU on this PC)"
    );
    Ok(c)
}

/// Where the running cut-out model is (`device`: `None` when not running):
/// the DirectML adapter's name, "CPU", or "".
pub fn cutout_device_text(adapter: Option<&str>, device: Option<p2c_segment::Device>) -> String {
    match device {
        Some(p2c_segment::Device::DirectMl) => adapter.unwrap_or("GPU").to_string(),
        Some(p2c_segment::Device::Cpu) => "CPU".into(),
        None => String::new(),
    }
}

/// The encoder line: the probed encoder, or x264 with the reason when a
/// hardware encoder failed at stream start (x264 is kept for the session).
pub fn encoder_text(probed: &str, fallback: Option<String>) -> String {
    match fallback {
        Some(why) => format!("x264 ({why})"),
        None => probed.to_string(),
    }
}

/// Longest fallback reason shown in the admin page, in characters.
const MAX_REASON_CHARS: usize = 120;

/// R31: the video status for the admin page: a fallback reason cut to its
/// first line and at most 120 characters (the log keeps it all).
pub fn display_video(s: &str) -> String {
    let short = |r: &str| {
        let line = r.lines().next().unwrap_or("");
        if line.chars().count() <= MAX_REASON_CHARS {
            line.to_string()
        } else {
            let mut t: String = line.chars().take(MAX_REASON_CHARS - 1).collect();
            t.push('…');
            t
        }
    };
    const PREFIX: &str = "CPU (fallback: ";
    match s.strip_prefix(PREFIX) {
        Some(rest) => {
            let reason = rest.strip_suffix(')').unwrap_or(rest);
            format!("{PREFIX}{})", short(reason))
        }
        None => short(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2c_compositor::{BoxBackdrop, FullBackdrop};

    #[test]
    fn set_crop_saves_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("page2cam.toml");
        std::fs::write(&p, "# keep me\nfps = 25\n").unwrap();
        let shared: p2c_core::SharedCrop = Arc::new(RwLock::new(p2c_core::Crop::FULL));
        let live = p2c_core::Crop {
            x: 0.25,
            y: 0.25,
            w: 0.5,
            h: 0.5,
        };
        // Live update: applied at once, file untouched.
        assert_eq!(change_crop(&shared, Some(&p), live, false), live);
        assert_eq!(*shared.read(), live);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("webcam_crop"), "{text}");
        // Final update: clamped (w 0.01 -> 0.10), saved, comments kept.
        let tiny = p2c_core::Crop {
            x: 0.0,
            y: 0.0,
            w: 0.01,
            h: 0.5,
        };
        let got = change_crop(&shared, Some(&p), tiny, true);
        assert!((got.w - 0.10).abs() < 1e-6, "{got:?}");
        assert_eq!(*shared.read(), got);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# keep me"), "{text}");
        let c: crate::config::Config = toml::from_str(&text).unwrap();
        assert_eq!(c.crop(), got);
        // No settings file: still applied.
        assert_eq!(change_crop(&shared, None, live, true), live);
        assert_eq!(*shared.read(), live);
    }

    #[test]
    fn restart_needed_when_saved_choice_differs() {
        assert!(!restart_needed("auto", "auto"));
        assert!(!restart_needed(" auto ", "auto"));
        assert!(!restart_needed("Intel(R) Graphics", "Intel(R) Graphics"));
        assert!(restart_needed("cpu", "auto"));
        assert!(restart_needed("Intel(R) Graphics", "auto"));
        // A one-run `--gpu-override cpu` counts as the running choice.
        assert!(restart_needed("auto", "cpu"));
    }

    fn adapter(
        index: u32,
        name: &str,
        vendor_id: u32,
        perf_rank: u32,
        software: bool,
    ) -> p2c_d3d::AdapterInfo {
        p2c_d3d::AdapterInfo {
            index,
            name: name.into(),
            vendor_id,
            luid: 100 + index as u64,
            perf_rank,
            software,
        }
    }

    fn pairs(l: &[GpuInfo]) -> Vec<(&str, &str)> {
        l.iter()
            .map(|g| (g.name.as_str(), g.kind.as_str()))
            .collect()
    }

    #[test]
    fn gpu_list_auto_first_cpu_last_no_software() {
        let l = gpu_list(&[
            adapter(0, "Intel(R) Graphics", 0x8086, 1, false),
            adapter(
                1,
                "NVIDIA RTX 2000 Ada Generation Laptop GPU",
                0x10DE,
                0,
                false,
            ),
            adapter(2, "Microsoft Basic Render Driver", 0x1414, 2, true),
        ]);
        assert_eq!(
            pairs(&l),
            vec![
                ("auto", "auto"),
                ("Intel(R) Graphics", "onboard"),
                ("NVIDIA RTX 2000 Ada Generation Laptop GPU", "discrete"),
                ("cpu", "cpu"),
            ]
        );
        assert_eq!(
            pairs(&gpu_list(&[])),
            vec![("auto", "auto"), ("cpu", "cpu")]
        );
    }

    #[test]
    fn amd_is_discrete_only_when_ranked_first_among_several() {
        // A lone AMD APU (e.g. Ryzen 8600G) is onboard.
        let apu = gpu_list(&[adapter(0, "AMD Radeon(TM) Graphics", 0x1002, 0, false)]);
        assert_eq!(apu[1].kind, "onboard");
        let two = gpu_list(&[
            adapter(0, "AMD Radeon(TM) Graphics", 0x1002, 1, false),
            adapter(1, "AMD Radeon RX 7600", 0x1002, 0, false),
        ]);
        assert_eq!(
            pairs(&two)[1..3],
            [
                ("AMD Radeon(TM) Graphics", "onboard"),
                ("AMD Radeon RX 7600", "discrete")
            ]
        );
        // NVIDIA has no integrated GPUs on Windows PCs.
        let nv = gpu_list(&[adapter(0, "NVIDIA GeForce RTX 4070", 0x10DE, 0, false)]);
        assert_eq!(nv[1].kind, "discrete");
    }

    #[test]
    fn gpu_list_drops_duplicate_names() {
        let l = gpu_list(&[
            adapter(0, "NVIDIA GeForce RTX 4070", 0x10DE, 0, false),
            adapter(1, "NVIDIA GeForce RTX 4070", 0x10DE, 1, false),
        ]);
        assert_eq!(l.len(), 3);
    }

    #[test]
    fn gpu_choice_must_be_listed() {
        let l = gpu_list(&[adapter(0, "Intel(R) Graphics", 0x8086, 0, false)]);
        assert_eq!(check_gpu_choice("auto", &l).unwrap(), "auto");
        assert_eq!(check_gpu_choice(" cpu", &l).unwrap(), "cpu");
        assert_eq!(
            check_gpu_choice("Intel(R) Graphics", &l).unwrap(),
            "Intel(R) Graphics"
        );
        assert!(check_gpu_choice("Voodoo 5", &l).is_err());
        assert!(check_gpu_choice("", &l).is_err());
    }

    #[test]
    fn video_status_shortens_fallback_reason() {
        assert_eq!(
            display_video("GPU: Intel(R) Graphics"),
            "GPU: Intel(R) Graphics"
        );
        assert_eq!(
            display_video("CPU (fallback: no frames\nCaused by: x)"),
            "CPU (fallback: no frames)"
        );
        let long = format!("CPU (fallback: {})", "e".repeat(300));
        let d = display_video(&long);
        assert!(d.starts_with("CPU (fallback: eee"), "{d}");
        assert!(d.ends_with("…)"), "{d}");
        assert_eq!(d.chars().count(), "CPU (fallback: )".chars().count() + 120);
        // Non-ASCII stays on character boundaries.
        let d = display_video(&format!("CPU (fallback: {})", "é".repeat(200)));
        assert!(d.ends_with("é…)"), "{d}");
    }

    #[test]
    fn restart_before_startup_is_refused_and_leaves_no_flag() {
        let exit = crate::restart::ExitFlags::default();
        let e = begin_restart(&exit, None).unwrap_err();
        assert!(format!("{e}").contains("still starting"), "{e}");
        // A later Ctrl-C must not relaunch.
        assert!(!exit.restart.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn restart_after_quit_is_refused() {
        // Quit then "Restart now" within the close delay: exit 0, not 75.
        let exit = crate::restart::ExitFlags::default();
        exit.user_shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let e = begin_restart(&exit, Some(Box::new(move || tx.send(()).unwrap()))).unwrap_err();
        assert!(format!("{e}").contains("closing"), "{e}");
        assert!(!exit.restart.load(std::sync::atomic::Ordering::SeqCst));
        assert!(rx
            .recv_timeout(std::time::Duration::from_millis(800))
            .is_err());
    }

    /// An `App` without browser, webcam or segmenter.
    fn bare_app() -> App {
        let (page, mic) = (AudioRing::new(50), AudioRing::new(50));
        let mixer = Mixer::new(page, mic.clone(), MixSettings::default());
        App {
            browser: OnceLock::new(),
            layout: Arc::new(RwLock::new(PipLayout::default())),
            backdrop: Arc::default(),
            bg: Mutex::new(
                crate::background::BgState::from_config(
                    &crate::config::Config::default(),
                    "about:blank".into(),
                )
                .0,
            ),
            apply_lock: Mutex::new(()),
            bg_base: OnceLock::new(),
            data_dir: None,
            thumbs: crate::background::ThumbCache::new(Box::new(|_| {
                anyhow::bail!("no network in tests")
            })),
            crop: Arc::new(RwLock::new(p2c_core::Crop::FULL)),
            stream: StreamController::new(mixer.clone()),
            mixer,
            mic_ring: mic,
            mic: Mutex::new(None),
            mic_status: Mutex::new("off".into()),
            webcam_slot: None,
            webcam_status: p2c_webcam::WebcamStatus::new("none"),
            vcam_status: "ok: test".into(),
            vcam_live: p2c_vcam::CameraStatus::default(),
            stream_pix_fmt: "bgra",
            stream_url: Mutex::new(String::new()),
            width: 1280,
            height: 720,
            fps: 30,
            video_kbps: 4000,
            fallback_url: Mutex::new("about:blank".into()),
            log_file: String::new(),
            segmenter: None,
            settings_path: None,
            mic_delay_ms: Mutex::new(0),
            latencies: Mutex::new(Default::default()),
            pipeline_ms: Arc::default(),
            updater: Arc::new(crate::update::Updater::new(
                "0.0.0",
                false,
                Box::new(|| Ok(None)),
            )),
            video: Arc::new(Mutex::new("CPU (fallback: test)".into())),
            encoder: Arc::new(Mutex::new("x264".into())),
            gpu_saved: Mutex::new("auto".into()),
            gpu_running: "auto".into(),
            gpu_override: false,
            cutout_gpu: None,
            exit: Arc::default(),
            quit_main: Mutex::new(None),
            ticker_stats: Arc::default(),
            health_window: Mutex::new(Default::default()),
            page_fps: Mutex::new(crate::page_fps::PageQuality::new(30)),
            screen: crate::screen::ScreenShare::new(Box::new(
                crate::screen::tests::FakeScreens::default(),
            )),
            page_paused: Arc::default(),
            output_frame: FrameSlot::new(),
            overlay: crate::overlay::OverlayLayers::new(None, 30),
        }
    }

    /// `status()` (which calls `health()`) and `health()` run back to back
    /// and while the sampler runs, without self-deadlock (debug builds panic
    /// on a lock held 2 s).
    #[test]
    fn status_and_health_do_not_self_deadlock() {
        let app = Arc::new(bare_app());
        let (tx, rx) = std::sync::mpsc::channel();
        let a = app.clone();
        std::thread::spawn(move || {
            for _ in 0..20 {
                a.sample_health();
                let s = a.status();
                let h = a.health();
                assert_eq!(s.health, h);
                assert!(h.video_fallback && h.camera_status == "ok: test");
                assert_eq!(h.webcam_state, "off");
            }
            tx.send(()).unwrap();
        });
        rx.recv_timeout(std::time::Duration::from_secs(20))
            .expect("status/health deadlocked");
    }

    /// The debug guard turns a held lock into a panic instead of a hang.
    #[test]
    #[cfg(debug_assertions)]
    fn debug_lock_guard_panics_on_a_held_lock() {
        let app = Arc::new(bare_app());
        let held = app.video.lock();
        let a = app.clone();
        let r = std::thread::spawn(move || a.health()).join();
        drop(held);
        assert!(r.is_err(), "health() must panic while `video` is held");
    }

    #[test]
    fn restart_sets_flag_and_closes() {
        let exit = crate::restart::ExitFlags::default();
        let (tx, rx) = std::sync::mpsc::channel();
        begin_restart(&exit, Some(Box::new(move || tx.send(()).unwrap()))).unwrap();
        assert!(exit.restart.load(std::sync::atomic::Ordering::SeqCst));
        rx.recv_timeout(std::time::Duration::from_secs(3))
            .expect("closer not called");
    }

    #[test]
    fn cutout_device_names_the_gpu() {
        use p2c_segment::Device;
        let nv = Some("NVIDIA RTX 2000 Ada Generation Laptop GPU");
        assert_eq!(
            cutout_device_text(nv, Some(Device::DirectMl)),
            "NVIDIA RTX 2000 Ada Generation Laptop GPU"
        );
        assert_eq!(cutout_device_text(None, Some(Device::DirectMl)), "GPU");
        assert_eq!(cutout_device_text(nv, Some(Device::Cpu)), "CPU");
        // Not running: nothing to show.
        assert_eq!(cutout_device_text(nv, None), "");
    }

    #[test]
    fn encoder_status_shows_the_x264_fallback() {
        assert_eq!(encoder_text("NVENC", None), "NVENC");
        assert_eq!(
            encoder_text("NVENC", Some("NVENC failed at stream start".into())),
            "x264 (NVENC failed at stream start)"
        );
    }

    #[test]
    fn delay_ignores_one_slow_frame() {
        assert_eq!(smoothed_delay(&[31, 30, 300, 32, 31]), 31);
    }

    #[test]
    fn delay_is_capped() {
        assert_eq!(smoothed_delay(&[400; 5]), 250);
    }

    #[test]
    fn no_samples_no_delay() {
        assert_eq!(smoothed_delay(&[]), 0);
    }

    #[test]
    fn mic_delay_adds_pipeline_latency() {
        assert_eq!(mic_delay(&[30, 30, 30], 66), 96);
        // No cut-out: only the output pipeline delays the video.
        assert_eq!(mic_delay(&[], 66), 66);
        assert_eq!(mic_delay(&[], 0), 0);
    }

    const BG_BASE: &str = "http://127.0.0.1:8765/bg/0123456789abcdef0123456789abcdef/";

    /// A bare app with a settings file, data folder and background pages.
    fn bg_app(dir: &std::path::Path) -> App {
        let mut app = bare_app();
        std::fs::write(dir.join("page2cam.toml"), "# keep me\n").unwrap();
        app.settings_path = Some(dir.join("page2cam.toml"));
        app.data_dir = Some(dir.to_path_buf());
        app.start_background(BG_BASE.into());
        app
    }

    fn bg(full: &str, in_box: &str) -> BackgroundDto {
        BackgroundDto {
            full: full.into(),
            in_box: in_box.into(),
            ..BackgroundDto::default()
        }
    }

    fn settings(dir: &std::path::Path) -> crate::config::Config {
        let text = std::fs::read_to_string(dir.join("page2cam.toml")).unwrap();
        assert!(text.contains("# keep me"), "{text}");
        toml::from_str(&text).unwrap()
    }

    fn shown(app: &App) -> String {
        app.fallback_url.lock().clone()
    }

    #[test]
    fn last_page_url_never_becomes_a_background_url() {
        let dir = tempfile::tempdir().unwrap();
        let app = bg_app(dir.path());
        app.navigate("https://user.test/a");
        assert_eq!(shown(&app), "https://user.test/a");
        let youtube = BackgroundDto {
            full_youtube: Some("https://youtu.be/dQw4w9WgXcQ?t=3".into()),
            ..bg("youtube", "room")
        };
        let colour = BackgroundDto {
            full_colour: Some("#112233".into()),
            ..bg("colour", "room")
        };
        // Switching quickly between backgrounds: Chromium shows the
        // generated page; the admin page keeps the user's URL.
        app.set_background(youtube.clone()).unwrap();
        assert!(
            shown(&app).starts_with(&format!("{BG_BASE}current.html")),
            "{}",
            shown(&app)
        );
        assert_eq!(app.status().url, "https://user.test/a");
        app.set_background(colour.clone()).unwrap();
        assert!(shown(&app).starts_with(BG_BASE));
        app.set_background(youtube.clone()).unwrap();
        // Blurred room: the page is not drawn, but the video must stop.
        app.set_background(bg("blurred-room", "room")).unwrap();
        assert_eq!(shown(&app), "https://user.test/a");
        app.set_background(colour).unwrap();
        // Back to Page: the user's page again.
        app.set_background(bg("page", "room")).unwrap();
        assert_eq!(shown(&app), "https://user.test/a");
        assert_eq!(app.bg.lock().last_page_url, "https://user.test/a");
        assert_eq!(settings(dir.path()).last_page_url, "https://user.test/a");
        // The page moved on by itself (link click): remembered on leaving.
        *app.fallback_url.lock() = "https://user.test/b".into();
        app.set_background(youtube.clone()).unwrap();
        app.set_background(bg("page", "room")).unwrap();
        assert_eq!(shown(&app), "https://user.test/b");
        assert_eq!(settings(dir.path()).last_page_url, "https://user.test/b");
        // Navigating from the admin page while a background shows: the page
        // comes back with the new URL.
        app.set_background(youtube).unwrap();
        app.navigate("https://user.test/c");
        assert_eq!(shown(&app), "https://user.test/c");
        assert_eq!(app.background().full, "page");
        let c = settings(dir.path());
        assert_eq!(
            (c.last_page_url.as_str(), c.background.as_str()),
            ("https://user.test/c", "page")
        );
        // Non-page URLs are never remembered.
        app.navigate("about:blank");
        app.set_background(bg("blurred-room", "room")).unwrap();
        app.set_background(bg("page", "room")).unwrap();
        assert_eq!(app.bg.lock().last_page_url, "https://user.test/c");
        let text = std::fs::read_to_string(dir.path().join("page2cam.toml")).unwrap();
        assert!(!text.contains("/bg/"), "{text}");
    }

    /// The screen background: a monitor captured while Chromium's paints are
    /// paused; a black generated page underneath; saved with its monitor.
    #[test]
    fn screen_background_captures_with_the_page_paused() {
        use std::sync::atomic::Ordering::SeqCst;
        let dir = tempfile::tempdir().unwrap();
        let mut app = bg_app(dir.path());
        let fake = crate::screen::tests::FakeScreens::default();
        app.screen = crate::screen::ScreenShare::new(Box::new(fake.clone()));
        app.navigate("https://user.test/a");
        // Empty: the primary monitor, saved by its id.
        app.set_background(bg("screen", "room")).unwrap();
        assert_eq!(fake.live.load(SeqCst), 1);
        assert_eq!(app.screen.running().as_deref(), Some("DISPLAY1"));
        assert!(app.page_paused.load(SeqCst));
        assert!(
            shown(&app).starts_with(BG_BASE),
            "black page under the capture"
        );
        assert!(app.backdrop.read().hides_room);
        let c = settings(dir.path());
        assert_eq!(
            (c.background.as_str(), c.background_screen.as_str()),
            ("screen", "DISPLAY1")
        );
        assert_eq!(app.background().full_screen.as_deref(), Some("DISPLAY1"));
        // Another monitor by index: one capture, switched.
        let two = BackgroundDto {
            full_screen: Some("1".into()),
            ..bg("screen", "room")
        };
        app.set_background(two).unwrap();
        assert_eq!(*fake.starts.lock(), vec!["DISPLAY1", "DISPLAY2"]);
        assert_eq!(fake.live.load(SeqCst), 1);
        // An unknown monitor changes nothing.
        let bad = BackgroundDto {
            full_screen: Some("DISPLAY9".into()),
            ..bg("screen", "room")
        };
        let e = app.set_background(bad).unwrap_err().to_string();
        assert!(e.contains("DISPLAY9"), "{e}");
        assert_eq!(app.screen.running().as_deref(), Some("DISPLAY2"));
        // Admin navigation: back to the page, capture stopped, paints resumed.
        app.navigate("https://user.test/b");
        assert_eq!(fake.live.load(SeqCst), 0);
        assert!(!app.page_paused.load(SeqCst));
        assert_eq!(app.background().full, "page");
        assert_eq!(shown(&app), "https://user.test/b");
        // The last monitor is offered again.
        app.set_background(bg("screen", "room")).unwrap();
        assert_eq!(app.screen.running().as_deref(), Some("DISPLAY2"));
        app.set_background(bg("page", "room")).unwrap();
        assert_eq!(fake.live.load(SeqCst), 0);
    }

    #[test]
    fn screen_background_failures_are_explained() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = bg_app(dir.path());
        let fake = crate::screen::tests::FakeScreens::default();
        app.screen = crate::screen::ScreenShare::new(Box::new(fake.clone()));
        *fake.fail.lock() = true;
        app.set_background(bg("screen", "room")).unwrap();
        assert!(app.status().background_note.contains("monitor gone"));
        app.set_background(bg("page", "room")).unwrap();
        assert!(!app.status().background_note.contains("monitor gone"));
        // Linux: refused with the reason, nothing changed.
        app.screen = crate::screen::ScreenShare::new(Box::new(crate::screen::tests::FakeScreens {
            unsupported: true,
            ..Default::default()
        }));
        let e = app
            .set_background(bg("screen", "room"))
            .unwrap_err()
            .to_string();
        assert_eq!(e, p2c_screen::UNSUPPORTED);
        assert_eq!(app.background().full, "page");
        assert_eq!(app.screens().unwrap_err(), p2c_screen::UNSUPPORTED);
    }

    /// I1: a generated full frame tells the compositor the room is hidden,
    /// so a cut-out without a mask gives "cut-out not ready".
    #[test]
    fn generated_full_frame_reports_cutout_not_ready() {
        let dir = tempfile::tempdir().unwrap();
        let app = bg_app(dir.path());
        let colour = BackgroundDto {
            full_colour: Some("#112233".into()),
            ..bg("colour", "room")
        };
        app.set_background(colour).unwrap();
        let b = app.backdrop.read().clone();
        assert!(b.hides_room);
        let note = p2c_compositor::ComposeNote::of(&b, &app.pip(), true, false);
        assert_eq!(note, p2c_compositor::ComposeNote::CutoutNotReady);
        app.set_background(bg("page", "room")).unwrap();
        assert!(!app.backdrop.read().hides_room);
    }

    #[test]
    fn box_switch_resets_full_background() {
        let dir = tempfile::tempdir().unwrap();
        let app = bg_app(dir.path());
        app.navigate("https://user.test/a");
        let colour = BackgroundDto {
            full_colour: Some("#112233".into()),
            ..bg("colour", "blur")
        };
        app.set_background(colour).unwrap();
        assert_eq!(app.pip().style, OverlayStyle::CutoutFull);
        // The operator picks Box again: the full frame goes back to the page.
        app.set_pip(PipLayout {
            style: OverlayStyle::Box,
            ..app.pip()
        });
        assert_eq!(app.pip().style, OverlayStyle::Box);
        let b = app.background();
        assert_eq!((b.full.as_str(), b.in_box.as_str()), ("page", "blur"));
        assert_eq!(shown(&app), "https://user.test/a");
        assert_eq!(app.backdrop.read().full, FullBackdrop::Page);
        assert_eq!(app.backdrop.read().in_box, BoxBackdrop::Blur);
        let note = app.status().background_note;
        assert!(note.contains("Page"), "{note}");
        let c = settings(dir.path());
        assert_eq!(
            (c.background.as_str(), c.overlay_style.as_str()),
            ("page", "box")
        );
        assert_eq!(c.box_background, "blur");
    }

    #[test]
    fn full_background_in_box_switches_to_cutout_full_and_saves() {
        let dir = tempfile::tempdir().unwrap();
        let app = bg_app(dir.path());
        assert_eq!(app.pip().style, OverlayStyle::Box);
        app.set_background(BackgroundDto {
            refined_edges: true,
            ..bg("blurred-room", "room")
        })
        .unwrap();
        assert_eq!(app.pip().style, OverlayStyle::CutoutFull);
        assert_eq!(app.backdrop.read().full, FullBackdrop::BlurredRoom);
        assert!(app.backdrop.read().refined_edges);
        let s = app.status();
        assert!(
            s.background_note.contains("cut-out"),
            "{}",
            s.background_note
        );
        assert_eq!(
            (
                s.background.as_str(),
                s.box_background.as_str(),
                s.refined_edges
            ),
            ("blurred-room", "room", true)
        );
        let c = settings(dir.path());
        assert_eq!(c.overlay_style, "cutout-full");
        assert_eq!(c.background, "blurred-room");
        assert!(c.refined_edges);
        // Already a cut-out style: kept as it is.
        app.set_pip(PipLayout {
            style: OverlayStyle::CutoutSmall,
            ..app.pip()
        });
        app.set_background(BackgroundDto {
            full_colour: Some("#ABCDEF".into()),
            ..bg("colour", "room")
        })
        .unwrap();
        assert_eq!(app.pip().style, OverlayStyle::CutoutSmall);
        let c = settings(dir.path());
        assert_eq!(c.overlay_style, "cutout-small");
        assert_eq!(
            (c.background.as_str(), c.background_colour.as_str()),
            ("colour", "#abcdef")
        );
        assert!(app
            .background_page()
            .unwrap()
            .contains("background:#abcdef"));
        // Errors change nothing.
        for bad in [
            BackgroundDto {
                full_youtube: Some("https://youtube.com/playlist?list=PL1".into()),
                ..bg("youtube", "room")
            },
            BackgroundDto {
                full_colour: Some("blue".into()),
                ..bg("colour", "room")
            },
            bg("image", "room"),
            bg("aquarium", "room"),
            bg("page", "fog"),
        ] {
            assert!(app.set_background(bad.clone()).is_err(), "{bad:?}");
        }
        assert_eq!(app.background().full, "colour");
        assert_eq!(app.background().full_colour.as_deref(), Some("#abcdef"));
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 10, 20]));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn uploaded_images_are_stored_and_shown() {
        let dir = tempfile::tempdir().unwrap();
        let app = bg_app(dir.path());
        // In the box: decoded for the compositor, saved re-encoded.
        app.set_background_image(ImageRow::Box, png(64, 32))
            .unwrap();
        assert!(matches!(app.backdrop.read().in_box, BoxBackdrop::Image(ref f) if f.width == 64));
        let c = settings(dir.path());
        assert_eq!(c.box_background, "image");
        let stored = std::path::PathBuf::from(&c.box_background_image);
        assert_eq!(stored, dir.path().join("backgrounds").join("box.jpg"));
        assert!(stored.is_file());
        // Full frame: a page showing /bg/<id>/image, served re-encoded.
        app.set_background_image(ImageRow::Full, png(64, 32))
            .unwrap();
        assert_eq!(app.background().full, "image");
        assert!(app.background().full_image);
        let page = app.background_page().unwrap();
        assert!(page.contains("<img src=\"image?v="), "{page}");
        let (bytes, kind) = app.background_image(ImageRow::Full).unwrap();
        assert_eq!(kind, "image/jpeg");
        assert_eq!(&bytes[..2], &[0xff, 0xd8]);
        // Not an image: refused, nothing replaced.
        assert!(app
            .set_background_image(ImageRow::Full, b"<html>".to_vec())
            .is_err());
        assert_eq!(app.background_image(ImageRow::Full).unwrap().0, bytes);
        // Saved choices come back at the next start.
        let (st, warn) =
            crate::background::BgState::from_config(&settings(dir.path()), "https://x.test".into());
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(st.full, crate::background::FullBg::Image);
        assert_eq!(st.in_box, crate::background::BoxBg::Image);
        assert_eq!(st.box_image.unwrap().frame.width, 64);
    }

    #[test]
    fn youtube_thumb_uses_the_injected_fetcher() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = bg_app(dir.path());
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let c = calls.clone();
        app.thumbs = crate::background::ThumbCache::new(Box::new(move |u| {
            c.lock().push(u.to_string());
            Ok(vec![0xff, 0xd8, 0xff, 0xd9])
        }));
        assert!(app.youtube_thumb(None).is_err()); // no video chosen yet
        app.youtube_thumb(Some("https://youtu.be/dQw4w9WgXcQ".into()))
            .unwrap();
        assert!(app.youtube_thumb(Some("hello".into())).is_err());
        app.set_background(BackgroundDto {
            full_youtube: Some("dQw4w9WgXcQ".into()),
            ..bg("youtube", "room")
        })
        .unwrap();
        app.youtube_thumb(None).unwrap();
        assert_eq!(
            *calls.lock(),
            vec!["https://img.youtube.com/vi/dQw4w9WgXcQ/mqdefault.jpg".to_string()]
        );
    }

    /// Shown page and choices agree: a generated page exactly when the full
    /// frame is one, never Box with a full-frame background.
    fn assert_consistent(app: &App) {
        let full = app.bg.lock().full.clone();
        let style = app.pip().style;
        assert!(
            !(style == OverlayStyle::Box && full != crate::background::FullBg::Page),
            "Box with {full:?}"
        );
        let shown = shown(app);
        if full.is_generated() {
            assert!(shown.starts_with(BG_BASE), "{full:?} but showing {shown}");
        } else {
            assert_eq!(shown, app.bg.lock().last_page_url, "{full:?}");
        }
    }

    #[test]
    fn concurrent_background_changes_stay_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = bg_app(dir.path());
        app.settings_path = None; // speed: no file writes
        let app = Arc::new(app);
        app.navigate("https://user.test/a");
        let youtube = BackgroundDto {
            full_youtube: Some("dQw4w9WgXcQ".into()),
            ..bg("youtube", "room")
        };
        for round in 0..300 {
            app.set_pip(PipLayout {
                style: OverlayStyle::CutoutSmall,
                ..app.pip()
            });
            app.set_background(bg("page", "room")).unwrap();
            let barrier = Arc::new(std::sync::Barrier::new(3));
            let (a, b1) = (app.clone(), barrier.clone());
            let y = youtube.clone();
            let t1 = std::thread::spawn(move || {
                b1.wait();
                a.set_background(y).unwrap();
            });
            let (a, b2) = (app.clone(), barrier.clone());
            let t2 = std::thread::spawn(move || {
                b2.wait();
                a.set_pip(PipLayout {
                    style: OverlayStyle::Box,
                    ..a.pip()
                });
            });
            let (a, b3) = (app.clone(), barrier);
            let colour = BackgroundDto {
                full_colour: Some("#123456".into()),
                ..bg(if round % 2 == 0 { "colour" } else { "page" }, "room")
            };
            let t3 = std::thread::spawn(move || {
                b3.wait();
                a.set_background(colour).unwrap();
            });
            for t in [t1, t2, t3] {
                t.join().unwrap();
            }
            assert_consistent(&app);
        }
    }

    /// I2: the user's page is saved whenever it changes on Page (admin
    /// navigation, Back/Forward and the page's own navigations, seen when
    /// the shown URL changes), at most once per 2 s; a /bg/ URL never is.
    #[test]
    fn page_changes_on_page_save_last_page_url() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let app = bg_app(dir.path());
        let saved = || settings(dir.path()).last_page_url;
        // Taken before `navigate` stamps its write with the real clock, so a
        // slow settings write can't push that stamp before t0.
        let t0 = Instant::now();
        app.navigate("https://user.test/a");
        assert_eq!(app.bg.lock().last_page_url, "https://user.test/a");
        assert_eq!(saved(), "https://user.test/a");
        // The page moves by itself (link click, Back/Forward): remembered at
        // once, written once 2 s have passed since the last write.
        app.remember_page_url("https://user.test/b", t0);
        assert_eq!(app.bg.lock().last_page_url, "https://user.test/b");
        app.remember_page_url("https://user.test/c", t0 + Duration::from_millis(500));
        assert_eq!(app.bg.lock().last_page_url, "https://user.test/c");
        let written = saved();
        app.remember_page_url("https://user.test/c", t0 + Duration::from_millis(1900));
        assert_eq!(saved(), written, "more than one write within 2 s");
        app.remember_page_url("https://user.test/c", t0 + Duration::from_millis(4000));
        assert_eq!(saved(), "https://user.test/c", "pending write not flushed");
        // The watchdog poll sees the shown URL change.
        *app.fallback_url.lock() = "https://user.test/d".into();
        app.keep_page_off_background();
        assert_eq!(app.bg.lock().last_page_url, "https://user.test/d");
        // A /bg/ URL or a non-page URL never becomes the user's page.
        let later = Instant::now() + Duration::from_secs(10);
        for (i, u) in [format!("{BG_BASE}current.html?v=7"), "about:blank".into()]
            .iter()
            .enumerate()
        {
            app.remember_page_url(u, later + Duration::from_secs(3 * i as u64));
            assert_eq!(app.bg.lock().last_page_url, "https://user.test/d");
        }
        app.remember_page_url("https://user.test/d", later + Duration::from_secs(9));
        assert_eq!(saved(), "https://user.test/d");
        // While a generated background shows, nothing is remembered from
        // what Chromium shows.
        app.set_background(bg("colour", "room")).unwrap();
        app.remember_page_url("https://user.test/e", later + Duration::from_secs(20));
        assert_eq!(app.bg.lock().last_page_url, "https://user.test/d");
        assert_eq!(saved(), "https://user.test/d");
    }

    #[test]
    fn back_and_forward_never_leave_a_background_page_showing() {
        let dir = tempfile::tempdir().unwrap();
        let app = bg_app(dir.path());
        app.navigate("https://user.test/a");
        app.set_background(BackgroundDto {
            full_youtube: Some("dQw4w9WgXcQ".into()),
            ..bg("youtube", "room")
        })
        .unwrap();
        // While a background shows, Back/Forward are ignored (Reload isn't).
        assert!(!app.nav_allowed(NavAction::Back));
        assert!(!app.nav_allowed(NavAction::Forward));
        assert!(app.nav_allowed(NavAction::Reload));
        // On the page: allowed, but if history lands on a /bg/ page, the
        // user's page is loaded instead.
        app.set_background(bg("page", "room")).unwrap();
        assert!(app.nav_allowed(NavAction::Back));
        let back_landed = format!("{BG_BASE}current.html?v=2");
        *app.fallback_url.lock() = back_landed.clone();
        // Our own navigation may still be loading: left alone.
        app.keep_page_off_background();
        assert_eq!(shown(&app), back_landed);
        app.nav(NavAction::Back);
        app.keep_page_off_background();
        assert_eq!(shown(&app), "https://user.test/a");
        // A user page is left alone, and so is a generated page while chosen.
        *app.fallback_url.lock() = "https://user.test/b".into();
        app.keep_page_off_background();
        assert_eq!(shown(&app), "https://user.test/b");
        app.set_background(bg("blurred-room", "room")).unwrap();
        app.nav(NavAction::Back);
        app.keep_page_off_background();
        assert_eq!(shown(&app), "https://user.test/b");
    }

    #[test]
    fn background_watchdog_acts_only_after_history_moves() {
        let dir = tempfile::tempdir().unwrap();
        let app = bg_app(dir.path());
        app.navigate("https://user.test/a");
        app.set_background(bg("colour", "room")).unwrap();
        // Back to Page: our load of the user's page is slow (VPN), so
        // Chromium still shows the background page well past any grace.
        let still_bg = shown(&app);
        assert!(still_bg.starts_with(BG_BASE), "{still_bg}");
        app.set_background(bg("page", "room")).unwrap();
        *app.fallback_url.lock() = still_bg.clone();
        for _ in 0..5 {
            app.keep_page_off_background();
        }
        assert_eq!(
            shown(&app),
            still_bg,
            "a slow load must not be re-navigated"
        );
        // Reload doesn't move through history: still left alone.
        app.nav(NavAction::Reload);
        app.keep_page_off_background();
        assert_eq!(shown(&app), still_bg, "Reload must not arm the watchdog");
        // Back/Forward after our load lands on a background page while the
        // full frame is not one: the user's page is loaded.
        for a in [NavAction::Back, NavAction::Forward] {
            app.nav(a);
            *app.fallback_url.lock() = still_bg.clone();
            app.keep_page_off_background();
            assert_eq!(shown(&app), "https://user.test/a", "{a:?}");
            // That load is ours again: no loop while it is slow.
            *app.fallback_url.lock() = still_bg.clone();
            app.keep_page_off_background();
            assert_eq!(shown(&app), still_bg, "{a:?}: reload loop");
        }
        // The page moves through history by itself (history.back()): the
        // load committed (the URL changed), then a /bg/ URL shows.
        *app.fallback_url.lock() = "https://user.test/a".into();
        app.keep_page_off_background();
        *app.fallback_url.lock() = still_bg.clone();
        app.keep_page_off_background();
        assert_eq!(shown(&app), "https://user.test/a", "page-driven history");
        // ... and from another user page onto a different /bg/ URL: the page
        // it last showed (remembered when the shown URL changed) comes back.
        *app.fallback_url.lock() = "https://user.test/b".into();
        app.keep_page_off_background();
        let other_bg = format!("{BG_BASE}current.html?v=99");
        *app.fallback_url.lock() = other_bg;
        app.keep_page_off_background();
        assert_eq!(shown(&app), "https://user.test/b", "page-driven history 2");
    }

    #[test]
    fn input_follows_a_lowered_page_size() {
        use p2c_admin::{InputEvent as E, MouseBtn};
        let (full, half) = ((1920, 1080), (960, 540));
        let click = E::Button {
            x: 1919,
            y: 540,
            button: MouseBtn::Left,
            down: true,
            clicks: 1,
            mods: 0,
        };
        assert_eq!(scale_input(click, full, full), click);
        match scale_input(click, full, half) {
            E::Button { x, y, .. } => assert_eq!((x, y), (959, 270)),
            e => panic!("{e:?}"),
        }
        match scale_input(
            E::Move {
                x: 960,
                y: 1079,
                mods: 0,
            },
            full,
            (1280, 720),
        ) {
            E::Move { x, y, .. } => assert_eq!((x, y), (640, 719)),
            e => panic!("{e:?}"),
        }
        match scale_input(
            E::Wheel {
                x: 100,
                y: 100,
                dx: 0,
                dy: -120,
                mods: 0,
            },
            full,
            half,
        ) {
            E::Wheel { x, y, dy, .. } => assert_eq!((x, y, dy), (50, 50, -60)),
            e => panic!("{e:?}"),
        }
        let key = E::Key {
            action: p2c_admin::KeyAction::Char,
            code: 65,
            ch: 97,
            mods: 0,
        };
        assert_eq!(scale_input(key, full, half), key);
    }

    #[test]
    fn mic_delay_capped_at_250() {
        assert_eq!(MAX_MIC_DELAY_MS, 250);
        assert_eq!(mic_delay(&[200; 5], 100), 250);
        assert_eq!(mic_delay(&[], 400), 250);
    }
}
