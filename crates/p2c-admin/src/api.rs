//! What the admin UI can ask of the app, and the JSON shapes it uses.
use p2c_compositor::{OverlayStyle, PipLayout};
use p2c_core::Crop;
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

/// Webcam crop as JSON: a rectangle in 0..1 of the camera picture.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CropDto {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Default for CropDto {
    fn default() -> Self {
        Crop::FULL.into()
    }
}

impl From<Crop> for CropDto {
    fn from(c: Crop) -> Self {
        CropDto {
            x: c.x,
            y: c.y,
            w: c.w,
            h: c.h,
        }
    }
}

impl From<CropDto> for Crop {
    fn from(c: CropDto) -> Self {
        Crop {
            x: c.x,
            y: c.y,
            w: c.w,
            h: c.h,
        }
    }
}

impl CropDto {
    /// Through [`Crop::clamp`]: at least 10 % wide/high, inside the frame, NaN → full.
    pub fn clamped(self) -> CropDto {
        Crop::from(self).clamp().into()
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
    pub update: UpdateInfo,
    /// What makes the video, e.g. "GPU: NVIDIA RTX 2000 Ada" or
    /// "CPU (fallback: <short reason>)".
    pub video: String,
    /// Stream encoder: "NVENC" | "Quick Sync" | "AMF" | "x264" | "probing…".
    pub encoder: String,
    /// Saved `gpu` setting: "auto" | "cpu" | an adapter name.
    pub gpu_choice: String,
    /// The `gpu` setting this run started with (same forms).
    pub gpu_running: String,
    /// The saved choice differs from the running one: a restart applies it.
    pub restart_needed: bool,
    /// This run was started on the CPU because Chromium closed unexpectedly
    /// twice on the GPU (`--gpu-override cpu`): a restart tries the GPU again.
    pub gpu_override: bool,
    /// Where the cut-out model runs while it is in use: the GPU's name,
    /// "GPU", "CPU", or "" (cut-out not running).
    pub cutout_gpu: String,
    /// Health over the last ~10 s, for the tray's icon.
    pub health: Health,
}

/// Engine health for the tray (rates over a rolling ~10 s window).
/// Missing fields read as their defaults (a newer tray reading an older engine).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Health {
    /// The ticker has pushed its first output frame (the tray hands the
    /// camera over to the engine after this).
    pub output_ready: bool,
    /// Output ticks missed (late) in percent of all due ticks.
    pub output_skipped_pct: f32,
    /// Webcam frames per second arriving (0 without a webcam).
    pub webcam_fps: f32,
    /// "ok" | "reconnecting" | "error" | "off" (no webcam in use).
    pub webcam_state: String,
    /// The cut-out is in use and fell back from Quality to Fast by itself.
    pub cutout_auto_fallback: bool,
    /// Video moved to the CPU compositor after a GPU failure.
    pub video_fallback: bool,
    /// The virtual camera's status line (same as `Status::vcam`).
    pub camera_status: String,
}

/// One entry of the "Video processing" picker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuInfo {
    /// "auto", "cpu" or the exact adapter name (the value to save).
    pub name: String,
    /// "auto" | "discrete" | "onboard" | "cpu"
    pub kind: String,
}

/// Software update state for the banner.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct UpdateInfo {
    /// "" (up to date / not checked) | "available" | "installing" | "error: <why>"
    pub state: String,
    /// Newer version on offer, e.g. "0.2.0".
    pub version: String,
    /// Release page (for portable copies, which update by hand).
    pub page_url: String,
    /// True when this copy was installed with the installer and can update itself.
    pub can_install: bool,
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
    /// Download, verify and run the newer installer (the app then closes).
    fn install_update(&self) -> anyhow::Result<()>;
    /// The picker's choices: "auto" first, each hardware GPU, "cpu" last.
    fn gpus(&self) -> Vec<GpuInfo>;
    /// Save the `gpu` setting ("auto", "cpu" or a listed adapter name); it
    /// applies after a restart. Errors on anything else.
    fn set_gpu_choice(&self, choice: &str) -> anyhow::Result<()>;
    /// Close and start again (same command line); returns before closing.
    fn restart(&self) -> anyhow::Result<()>;
    /// Close normally (as Ctrl-C does); returns before closing.
    fn quit(&self);
    /// The webcam crop in use.
    fn crop(&self) -> CropDto;
    /// Apply a crop (clamped through `Crop::clamp`) from the next webcam
    /// frame on; `save` also writes it to the settings file.
    fn set_crop(&self, c: CropDto, save: bool);
}
