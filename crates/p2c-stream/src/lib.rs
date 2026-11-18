//! RTMP streaming through an ffmpeg child process.
mod args;
mod controller;

pub use args::{
    candidates, encoder_options, ffmpeg_args, find_ffmpeg, probe, probe_args, redact,
    retry_with_x264, target_url, Encoder, StreamConfig,
};
pub use controller::{StreamController, StreamSink, StreamState};
