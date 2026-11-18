//! What the admin UI can ask of the app, and the JSON shapes it uses.
use p2c_compositor::{OverlayStyle, PipLayout};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NavAction {
    Back,
    Forward,
    Reload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseBtn {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Down,
    Up,
    Char,
}

/// Input for the page, in page pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputEvent {
    Move {
        x: i32,
        y: i32,
        mods: u32,
    },
    Button {
        x: i32,
        y: i32,
        button: MouseBtn,
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
    Key {
        action: KeyAction,
        code: i32,
        ch: u16,
        mods: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct StreamRequest {
    /// RTMP server URL, e.g. rtmp://a.rtmp.youtube.com/live2
    pub url: String,
    /// Stream key. Empty: use the key saved in the OS keyring.
    #[serde(default)]
    pub key: String,
    /// Save the key in the OS keyring.
    #[serde(default)]
    pub remember: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioSettings {
    pub page_on: bool,
    pub page_gain: f32,
    pub mic_on: bool,
    pub mic_gain: f32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        AudioSettings {
            page_on: true,
            page_gain: 1.0,
            mic_on: false,
            mic_gain: 1.0,
        }
    }
}

/// PIP layout as JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PipDto {
    pub visible: bool,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    /// "box" | "cutout-small" | "cutout-full"
    #[serde(default = "default_style")]
    pub style: String,
}

fn default_style() -> String {
    OverlayStyle::Box.as_str().into()
}

impl From<PipLayout> for PipDto {
    fn from(l: PipLayout) -> Self {
        PipDto {
            visible: l.visible,
            x: l.x,
            y: l.y,
            width: l.width,
            style: l.style.as_str().into(),
        }
    }
}

impl TryFrom<PipDto> for PipLayout {
    type Error = anyhow::Error;

    fn try_from(d: PipDto) -> anyhow::Result<Self> {
        let style = OverlayStyle::parse(&d.style)
            .ok_or_else(|| anyhow::anyhow!("unknown overlay style {:?}", d.style))?;
        Ok(PipLayout {
            visible: d.visible,
            x: d.x,
            y: d.y,
            width: d.width.clamp(0.05, 0.5),
            style,
        })
    }
}

/// Everything the status bar and panels show. Never contains the stream key.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// "ok: <desc>" | "error: <why>"
    pub vcam: String,
    /// "ok: <name>" | "none" | "error: <why>"
    pub webcam: String,
    /// Webcam frame aspect (h / w), for drawing the PIP box in the preview.
    pub webcam_aspect: f32,
    /// "idle" | "connecting" | "live" | "error: <why>"
    pub stream: String,
    pub stream_url: String,
    pub has_saved_key: bool,
    /// "ok" | "none" | "error: <why>"
    pub mic: String,
    pub log_file: String,
    /// Cut-out status line, e.g. "Quality · CPU · 24 masks/s", or why it isn't running.
    pub cutout: String,
    /// "quality" | "fast"
    pub cutout_engine: String,
}

/// The app side of the admin UI. Implementations must be cheap and non-blocking.
pub trait Controls: Send + Sync + 'static {
    fn navigate(&self, url: &str);
    fn nav(&self, a: NavAction);
    fn input(&self, ev: InputEvent);
    fn pip(&self) -> PipLayout;
    fn set_pip(&self, l: PipLayout);
    fn start_stream(&self, req: StreamRequest) -> anyhow::Result<()>;
    fn stop_stream(&self);
    fn audio(&self) -> AudioSettings;
    fn set_audio(&self, a: AudioSettings);
    fn status(&self) -> Status;
    /// "quality" | "fast": the operator's cut-out engine choice.
    fn cutout_engine(&self) -> String;
    /// Errors on an unknown engine name.
    fn set_cutout_engine(&self, engine: &str) -> anyhow::Result<()>;
}
