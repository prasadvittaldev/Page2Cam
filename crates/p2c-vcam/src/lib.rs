//! Virtual camera output: pixel conversion and platform sinks.
use std::path::{Path, PathBuf};

use p2c_compositor::{FrameSink, OutputSpec};

mod convert;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows_cam;

pub use convert::bgra_to_yuyv;
#[cfg(target_os = "linux")]
pub use linux::{find_loopback_device, resolve_device, V4l2Sink};

/// An opened virtual camera.
pub struct VirtualCamera {
    pub sink: Box<dyn FrameSink>,
    /// Human-readable description for logs and the admin status.
    pub description: String,
    /// Our own output device node, so the webcam picker can exclude it.
    pub own_device: Option<PathBuf>,
}

/// Open this platform's virtual camera. `device` overrides auto-detection where
/// the platform has device nodes (Linux).
pub fn open_default(device: Option<&Path>, spec: &OutputSpec) -> anyhow::Result<VirtualCamera> {
    #[cfg(target_os = "linux")]
    {
        let dev = resolve_device(device, Path::new("/sys"))?;
        let sink = V4l2Sink::open(&dev, spec)?;
        Ok(VirtualCamera {
            sink: Box::new(sink),
            description: format!("v4l2loopback {}", dev.display()),
            own_device: Some(dev),
        })
    }
    #[cfg(windows)]
    {
        let _ = device;
        let sink = windows_cam::WinVcamSink::open(spec)?;
        Ok(VirtualCamera {
            sink: Box::new(sink),
            description: "Windows camera \"Page2Cam\"".into(),
            own_device: None,
        })
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (device, spec);
        anyhow::bail!("virtual camera not available on this platform")
    }
}
