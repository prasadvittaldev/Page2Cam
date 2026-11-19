//! Virtual camera output: pixel conversion and platform sinks.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use p2c_compositor::{FrameSink, OutputSpec};

mod convert;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows_cam;

pub use convert::bgra_to_yuyv;
#[cfg(target_os = "linux")]
pub use linux::{find_loopback_device, resolve_device, V4l2Sink};
#[cfg(windows)]
pub use windows_cam::FrameWriter;

/// Admin status for a camera driver that wrote `dll_version` into the shared
/// memory (0 = a DLL from before the version field): `Some(error)` when it is
/// older than this app's frame format, else `None`.
pub fn camera_driver_status(dll_version: u32) -> Option<String> {
    (dll_version < p2c_shm::VERSION)
        .then(|| "error: camera driver out of date — reinstall Page2Cam".to_string())
}

/// Live camera status the sink updates while running (e.g. an out-of-date
/// driver noticed once Windows loads it). Cheap to clone; shared with the sink.
#[derive(Clone, Default)]
pub struct CameraStatus(Arc<Mutex<Option<String>>>);

impl CameraStatus {
    /// The current problem, if any (`None`: nothing beyond the open status).
    pub fn status(&self) -> Option<String> {
        self.0.lock().map(|g| g.clone()).unwrap_or(None)
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn set(&self, s: Option<String>) {
        if let Ok(mut g) = self.0.lock() {
            *g = s;
        }
    }
}

/// Who owns the Windows camera registration for a sink.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CameraMode {
    /// This process registers the camera and retries/recovers it (standalone).
    #[default]
    Own,
    /// The tray owns the camera; this sink only writes frames while the
    /// shared-memory `writer` field says `Engine`.
    FromTray,
}

/// An opened virtual camera.
pub struct VirtualCamera {
    pub sink: Box<dyn FrameSink>,
    /// Human-readable description for logs and the admin status.
    pub description: String,
    /// Our own output device node, so the webcam picker can exclude it.
    pub own_device: Option<PathBuf>,
    /// Problems found while running; overrides the open status when set.
    pub status: CameraStatus,
}

/// Open this platform's virtual camera. `device` overrides auto-detection where
/// the platform has device nodes (Linux).
pub fn open_default(device: Option<&Path>, spec: &OutputSpec) -> anyhow::Result<VirtualCamera> {
    open_default_mode(device, spec, CameraMode::Own)
}

/// Like `open_default`; `mode` only matters on Windows (Linux ignores it).
pub fn open_default_mode(
    device: Option<&Path>,
    spec: &OutputSpec,
    mode: CameraMode,
) -> anyhow::Result<VirtualCamera> {
    #[cfg(not(windows))]
    let _ = mode;
    #[cfg(target_os = "linux")]
    {
        let dev = resolve_device(device, Path::new("/sys"))?;
        let sink = V4l2Sink::open(&dev, spec)?;
        Ok(VirtualCamera {
            sink: Box::new(sink),
            description: format!("v4l2loopback {}", dev.display()),
            own_device: Some(dev),
            status: CameraStatus::default(),
        })
    }
    #[cfg(windows)]
    {
        let _ = device;
        let status = CameraStatus::default();
        let sink = windows_cam::WinVcamSink::open(spec, status.clone(), mode)?;
        Ok(VirtualCamera {
            sink: Box::new(sink),
            description: "Windows camera \"Page2Cam\"".into(),
            own_device: None,
            status,
        })
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (device, spec);
        anyhow::bail!("virtual camera not available on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vcam_status_reports_old_dll() {
        // A mapping created by an old DLL never had `dll_version` stamped.
        let mut mem = vec![0u64; p2c_shm::MAPPING_SIZE / 8 + 1];
        let base = mem.as_mut_ptr() as *mut u8;
        let v = unsafe { p2c_shm::dll_version(base) };
        assert_eq!(v, 0);
        let s = camera_driver_status(v).expect("old DLL reported");
        assert!(s.contains("out of date"), "{s}");
        // A current DLL stamps its version: no complaint.
        unsafe { p2c_shm::set_dll_version(base) };
        assert_eq!(
            camera_driver_status(unsafe { p2c_shm::dll_version(base) }),
            None
        );
    }
}
