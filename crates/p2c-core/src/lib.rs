//! Shared types for Page2Cam: video frames and the latest-frame slot.
mod audio;
mod frame;
mod slot;

pub use audio::{AudioRing, CHANNELS, SAMPLE_RATE};
pub use frame::{Frame, FrameError};
pub use slot::FrameSlot;
