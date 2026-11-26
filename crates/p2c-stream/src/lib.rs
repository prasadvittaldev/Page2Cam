//! RTMP streaming: one encoder ffmpeg, fanned out to a copy-only sender
//! ffmpeg per site.
mod args;
mod controller;
mod fanout;
mod sites;
mod ts;

pub use args::{
    candidates, encoder_args, encoder_options, find_ffmpeg, probe, probe_args, redact, redact_all,
    retry_with_x264, sender_args, Destination, Encoder, Site, StreamConfig,
};
pub use controller::{StreamController, StreamSink, StreamState};
pub use fanout::SENDER_QUEUE;
pub use sites::{classify_exit, overall, retry_delay, SenderExit, SiteState};
pub use ts::{is_random_access, TS};
