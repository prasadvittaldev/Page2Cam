//! Virtual camera output: pixel conversion and platform sinks.
mod convert;
#[cfg(target_os = "linux")]
mod linux;

pub use convert::bgra_to_yuyv;
#[cfg(target_os = "linux")]
pub use linux::{find_loopback_device, V4l2Sink};
