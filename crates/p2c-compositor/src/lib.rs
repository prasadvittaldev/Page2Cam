//! Fixed-rate compositing of the page (and later the webcam PIP) into output frames.
mod compose;
mod ticker;

pub use compose::compose;
pub use ticker::{FrameSink, OutputSpec, Ticker};
