//! Shared types for Page2Cam: video frames and the latest-frame slot.
mod frame;
mod slot;

pub use frame::{Frame, FrameError};
pub use slot::FrameSlot;
