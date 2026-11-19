//! Fixed-rate compositing of the page and the webcam overlay into output frames.
mod blur;
mod compose;
mod guided;
mod pip;
mod ticker;

pub use blur::BlurCache;
pub use compose::{fit_rect, scale_frame, scale_frame_straight, ComposeNote, Compositor, Pip};
pub use guided::{guided_upsample, GuidedScratch};
pub use pip::{pip_rect, Backdrop, BoxBackdrop, Corner, FullBackdrop, OverlayStyle, PipLayout};
pub use ticker::{
    CpuProducer, FallbackTrigger, FrameSink, Output, OutputFormat, OutputProducer, OutputSpec,
    ProducerEvent, TickInputs, Ticker, TickerStats,
};
