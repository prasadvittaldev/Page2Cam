//! RTMP streaming through an ffmpeg child process.
mod args;
mod controller;

pub use args::{
    candidates, ffmpeg_args, find_ffmpeg, probe, redact, target_url, Encoder, StreamConfig,
};
pub use controller::{StreamController, StreamSink, StreamState};
