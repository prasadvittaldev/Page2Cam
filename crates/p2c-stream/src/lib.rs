//! RTMP streaming through an ffmpeg child process.
mod args;
mod controller;
mod sites;

pub use args::{
    candidates, encoder_args, encoder_options, find_ffmpeg, probe, probe_args, redact, redact_all,
    retry_with_x264, sender_args, Destination, Encoder, Site, StreamConfig,
};
pub use controller::{StreamController, StreamSink, StreamState};
pub use sites::{classify_exit, overall, retry_delay, SenderExit, SiteState};
