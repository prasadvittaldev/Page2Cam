//! Fixed-rate compositing of the page (and later the webcam PIP) into output frames.
mod compose;
mod pip;
mod ticker;

pub use compose::{compose, scale_frame, scale_frame_straight};
pub use pip::{pip_rect, Corner, OverlayStyle, PipLayout};
pub use ticker::{FrameSink, OutputSpec, Ticker};
