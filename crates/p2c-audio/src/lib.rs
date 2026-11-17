//! Stream audio: page + microphone sources mixed to 48 kHz stereo f32.
mod mic;
mod mixer;
mod resample;

pub use mic::MicCapture;
pub use mixer::{MixSettings, Mixer};
pub use resample::Resampler;
