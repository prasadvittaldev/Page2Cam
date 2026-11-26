//! Stream audio: page + microphone sources mixed to 48 kHz stereo f32.
mod loopback;
mod mic;
mod mixer;
mod resample;

pub use loopback::ProcessLoopback;
pub use mic::MicCapture;
pub use mixer::{MixSettings, Mixer};
pub use resample::Resampler;
