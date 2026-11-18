//! The running app as seen by the admin page.
use std::sync::{Arc, OnceLock};

use p2c_admin::{
    AudioSettings, Controls, InputEvent, KeyAction, MouseBtn, NavAction, Status, StreamRequest,
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
    /// Mic delay currently applied, in ms.
    pub mic_delay_ms: Mutex<u32>,
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

    /// Delay the mic by the cut-out's processing time so lips match the
    /// voice on the stream. Changes under 20 ms are ignored to avoid churn.
    pub fn update_mic_delay(&self) {
        let want = match &self.segmenter {
            Some(seg) if self.layout.read().style.is_cutout() => {
                let st = seg.status();
                if st.active.is_some() {
                    st.latency_ms
                } else {
                    0
                }
            }
            _ => 0,
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
        Status {
            url,
            width: self.width,
            height: self.height,
            fps: self.fps,
            vcam: self.vcam_status.clone(),
            webcam: self.webcam_status.clone(),
            webcam_aspect,
            stream,
            stream_url: self.stream_url.lock().clone(),
            has_saved_key: Self::saved_key().is_some(),
            mic: self.mic_status.lock().clone(),
            log_file: self.log_file.clone(),
            cutout: self.cutout_status(),
            cutout_engine: self.cutout_engine(),
        }
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
