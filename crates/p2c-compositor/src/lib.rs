//! Fixed-rate compositing of the page and the webcam overlay into output frames.
mod blur;
mod compose;
mod guided;
mod pip;
mod ticker;

pub use blur::{blur_kernel, blur_low_size, BlurCache};
pub use compose::{
    cover_crop, fit_rect, plan_compose, refined_size, scale_frame, scale_frame_straight, Changed,
    ComposeNote, ComposePlan, Compositor, Pip, PipDraw, PlanBase, PlannedPip, RedrawWhy,
    GUIDED_EPS, GUIDED_RADIUS, REFINED_MAX_W,
};
pub use guided::{guided_upsample, GuidedScratch, GUIDED_SUBSAMPLE};
pub use pip::{pip_rect, Backdrop, BoxBackdrop, Corner, FullBackdrop, OverlayStyle, PipLayout};
pub use ticker::{
    CpuProducer, FallbackTrigger, FrameSink, Output, OutputFormat, OutputProducer, OutputSpec,
    OverlayFeed, ProducerEvent, TickInputs, Ticker, TickerStats,
};
