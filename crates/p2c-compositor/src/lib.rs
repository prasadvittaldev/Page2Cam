//! Fixed-rate compositing of the page and the webcam overlay into output frames.
mod compose;
mod pip;
mod ticker;

pub use compose::{fit_rect, scale_frame, scale_frame_straight, Compositor, Pip};
pub use pip::{pip_rect, Corner, OverlayStyle, PipLayout};
pub use ticker::{
    CpuProducer, FrameSink, Output, OutputFormat, OutputProducer, OutputSpec, TickInputs, Ticker,
};
