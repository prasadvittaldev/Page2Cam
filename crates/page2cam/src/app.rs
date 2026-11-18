//! The running app as seen by the admin page.
use std::sync::{Arc, OnceLock};

use p2c_admin::{
    AudioSettings, Controls, GpuInfo, InputEvent, KeyAction, MouseBtn, NavAction, Status,
    StreamRequest,
};
use p2c_audio::{MicCapture, MixSettings, Mixer};
use p2c_browser::{BrowserHandle, KeyInput, KeyKind, MouseButton};
use p2c_compositor::PipLayout;
use p2c_core::{AudioRing, FrameSlot};
use p2c_segment::{EngineChoice, Segmenter};
use p2c_stream::{StreamConfig, StreamController, StreamState};
use parking_lot::{Mutex, RwLock};

const KEYRING_SERVICE: &str = "page2cam";
const KEYRING_USER: &str = "stream-key";

pub struct App {
    pub browser: OnceLock<BrowserHandle>,
    pub layout: Arc<RwLock<PipLayout>>,
    pub stream: StreamController,
    pub mixer: Mixer,
    pub mic_ring: AudioRing,
    pub mic: Mutex<Option<MicCapture>>,
    pub mic_status: Mutex<String>,
    pub webcam_slot: Option<FrameSlot>,
    pub webcam_status: String,
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
    /// Why the browser loop ended (read by `main` after it returns).
    pub exit: Arc<crate::restart::ExitFlags>,
    /// Ends the wait in `--test-pattern` mode (no browser to shut down).
    pub quit_main: Mutex<Option<std::sync::mpsc::Sender<()>>>,
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
        if !self.layout.read().style.is_cutout() {
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

    /// Delay the mic by the cut-out's processing time plus the output pipeline's so lips match the
    /// voice on the stream: median of the last ~2 s of latencies, capped.
    /// Changes under 20 ms are ignored to avoid churn.
    pub fn update_mic_delay(&self) {
        let latest = match &self.segmenter {
            Some(seg) if self.layout.read().style.is_cutout() => {
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
}

impl Controls for App {
    fn navigate(&self, url: &str) {
        tracing::info!(url, "navigate");
        *self.fallback_url.lock() = url.to_string();
        self.with_browser(|b| b.navigate(url));
    }

    fn nav(&self, a: NavAction) {
        self.with_browser(|b| match a {
            NavAction::Back => b.back(),
            NavAction::Forward => b.forward(),
            NavAction::Reload => b.reload(),
        });
    }

    fn input(&self, ev: InputEvent) {
        self.with_browser(|b| match ev {
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
        });
    }

    fn pip(&self) -> PipLayout {
        *self.layout.read()
    }

    fn set_pip(&self, l: PipLayout) {
        let old = std::mem::replace(&mut *self.layout.write(), l);
        if old.style != l.style {
            tracing::info!(style = l.style.as_str(), "overlay style");
            self.save_choice("overlay_style", l.style.as_str());
        }
        if let Some(seg) = &self.segmenter {
            seg.set_enabled(l.visible && l.style.is_cutout());
        }
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
        let url = self
            .browser
            .get()
            .map(|b| b.current_url())
            .unwrap_or_else(|| self.fallback_url.lock().clone());
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
        let gpu_saved = self.gpu_saved.lock().clone();
        Status {
            url,
            width: self.width,
            height: self.height,
            fps: self.fps,
            vcam: self
                .vcam_live
                .status()
                .unwrap_or_else(|| self.vcam_status.clone()),
            webcam: self.webcam_status.clone(),
            webcam_aspect,
            stream,
            stream_url: self.stream_url.lock().clone(),
            has_saved_key: Self::saved_key().is_some(),
            mic: self.mic_status.lock().clone(),
            log_file: self.log_file.clone(),
            cutout: self.cutout_status(),
            cutout_engine: self.cutout_engine(),
            update: self.updater.info(),
            video: display_video(&self.video.lock()),
            encoder: encoder_text(&self.encoder.lock(), self.stream.encoder_fallback()),
            restart_needed: restart_needed(&gpu_saved, &self.gpu_running),
            gpu_choice: gpu_saved,
            gpu_running: self.gpu_running.clone(),
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
        // What ends the main loop: the browser, or (test pattern) the wait.
        let browser = self.browser.get().cloned();
        let quit_main = self.quit_main.lock().clone();
        let close: Option<Box<dyn FnOnce() + Send>> = match (browser, quit_main) {
            (Some(b), _) => Some(Box::new(move || b.shutdown())),
            (None, Some(q)) => Some(Box::new(move || {
                let _ = q.send(());
            })),
            (None, None) => None,
        };
        begin_restart(&self.exit, close)
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
}

/// "Restart now": set the restart flag and, shortly after (once the request
/// has been answered), call `close` to end the main loop; `main` then starts
/// the new process. Without a way to close yet (still starting up), refuse
/// without setting the flag, so a later Ctrl-C doesn't relaunch.
pub fn begin_restart(
    exit: &crate::restart::ExitFlags,
    close: Option<Box<dyn FnOnce() + Send>>,
) -> anyhow::Result<()> {
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
    fn restart_sets_flag_and_closes() {
        let exit = crate::restart::ExitFlags::default();
        let (tx, rx) = std::sync::mpsc::channel();
        begin_restart(&exit, Some(Box::new(move || tx.send(()).unwrap()))).unwrap();
        assert!(exit.restart.load(std::sync::atomic::Ordering::SeqCst));
        rx.recv_timeout(std::time::Duration::from_secs(3))
            .expect("closer not called");
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

    #[test]
    fn mic_delay_capped_at_250() {
        assert_eq!(MAX_MIC_DELAY_MS, 250);
        assert_eq!(mic_delay(&[200; 5], 100), 250);
        assert_eq!(mic_delay(&[], 400), 250);
    }
}
