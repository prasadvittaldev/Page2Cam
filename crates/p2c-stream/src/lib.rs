//! RTMP streaming through an ffmpeg child process.
mod args;
mod controller;

pub use args::{ffmpeg_args, find_ffmpeg, redact, target_url, StreamConfig};
pub use controller::{StreamController, StreamSink, StreamState};
