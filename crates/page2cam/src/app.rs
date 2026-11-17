//! The running app as seen by the admin page.
use std::sync::{Arc, OnceLock};

use p2c_admin::{
    AudioSettings, Controls, InputEvent, KeyAction, MouseBtn, NavAction, Status, StreamRequest,
};
use p2c_audio::{MicCapture, MixSettings, Mixer};
use p2c_browser::{BrowserHandle, KeyInput, KeyKind, MouseButton};
use p2c_compositor::PipLayout;
use p2c_core::{AudioRing, FrameSlot};
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
        *self.layout.write() = l;
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
        }
    }
}
