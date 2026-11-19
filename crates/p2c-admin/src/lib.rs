//! Local admin web UI: control panel, live preview and input forwarding.
//!
//! Talks to the app only through the [`Controls`] trait, so it can be tested
//! with a fake and doesn't link the browser engine.
mod api;
mod input;
mod preview;
mod server;
mod tray;

pub use api::{
    AudioSettings, Controls, CropDto, GpuInfo, Health, InputEvent, KeyAction, MouseBtn, NavAction,
    Status, StreamRequest, UpdateInfo,
};
pub use input::{map_preview, normalize_url};
pub use preview::{encode_snapshot, PreviewSink};
pub use server::{serve, AdminConfig, AdminServer, PortUnavailable};
pub use tray::TrayLink;
