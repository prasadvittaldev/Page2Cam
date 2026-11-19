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
pub use nv12::{bgra_to_nv12, camera_size, crop_nv12, Nv12Capture, Nv12Frame, Nv12Gate};
pub use slot::{FrameSlot, MaskSlot, Nv12Slot, Slot};
pub use stats::{DurationStats, RateWindow, Summary};
