//! Shared types for Page2Cam: video frames, cut-out masks, the latest-value
//! slot and timing statistics.
mod audio;
mod crop;
mod frame;
mod mask;
mod nv12;
mod slot;
mod stats;

pub use audio::{AudioRing, CHANNELS, SAMPLE_RATE};
pub use crop::{apply_crop, Crop, SharedCrop};
pub use frame::{Frame, FrameError};
pub use mask::{EdgeRange, Mask};
pub use nv12::{bgra_to_nv12, camera_size, Nv12Frame};
pub use slot::{FrameSlot, MaskSlot, Slot};
pub use stats::{DurationStats, RateWindow, Summary};
