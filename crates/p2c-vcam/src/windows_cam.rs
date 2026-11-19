//! Windows 11 virtual camera: registers "Page2Cam" with MFCreateVirtualCamera
//! (our COM media source DLL is then loaded by the Frame Server service) and
//! feeds it frames through the shared memory the DLL creates.
use std::time::{Duration, Instant};

use anyhow::Context;
use p2c_compositor::{FrameSink, Output, OutputSpec};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::MediaFoundation::{
    MFCreateVirtualCamera, MFShutdown, MFStartup, MFVirtualCameraAccess_CurrentUser,
    MFVirtualCameraLifetime_Session, MFVirtualCameraType_SoftwareCameraSource, MFSTARTUP_FULL,
    MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
};

use crate::{camera_driver_status, CameraStatus};

const NOT_INSTALLED: &str =
    "the Page2Cam camera is not installed. Close Page2Cam, right-click install-camera.cmd \
     and choose \"Run as administrator\", then start Page2Cam again";

fn is_registered() -> bool {
    let path = HSTRING::from(format!(
        "Software\\Classes\\CLSID\\{}\\InprocServer32",
        p2c_shm::CLSID_STR
    ));
    let mut key = HKEY::default();
    let ok = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, &path, None, KEY_READ, &mut key) }.is_ok();
    if ok {
        unsafe {
            let _ = RegCloseKey(key);
        }
    }
    ok
}

/// Owns the IMFVirtualCamera on a dedicated MTA thread; dropping it removes the camera.
struct Owner {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// How the camera owner is started (replaced in tests).
type StartFn = std::sync::Arc<dyn Fn() -> anyhow::Result<Owner> + Send + Sync>;

/// Minimum gap between attempts to start the camera after a failure.
const START_RETRY: Duration = Duration::from_secs(5);

/// True when no start was tried yet, or the last one was at least 5 s ago.
fn retry_due(last_try: Option<Instant>, now: Instant) -> bool {
    last_try.is_none_or(|t| now.saturating_duration_since(t) >= START_RETRY)
}

impl Owner {
    /// An owner with no camera behind it (tests).
    #[cfg(test)]
    fn dummy() -> Owner {
        Owner {
            stop: None,
            thread: None,
        }
    }

    fn start() -> anyhow::Result<Owner> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::Builder::new().name("p2c-wincam".into()).spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let result = (|| -> anyhow::Result<_> {
                MFStartup(MF_VERSION, MFSTARTUP_FULL).context("starting Media Foundation")?;
                let cam = MFCreateVirtualCamera(
                    MFVirtualCameraType_SoftwareCameraSource,
                    MFVirtualCameraLifetime_Session,
                    MFVirtualCameraAccess_CurrentUser,
                    &HSTRING::from(p2c_shm::CAMERA_NAME),
                    &HSTRING::from(p2c_shm::CLSID_STR),
                    None,
                )
                .context("creating the virtual camera (Windows 11 required)")?;
                cam.Start(None).context(
                    "starting the virtual camera. If this says access denied, re-run install-camera.cmd as administrator",
                )?;
                Ok(cam)
            })();
            match result {
                Ok(cam) => {
                    let _ = ready_tx.send(Ok(()));
                    let _ = stop_rx.recv();
                    let _ = cam.Remove();
                    drop(cam);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
            let _ = MFShutdown();
            CoUninitialize();
        })?;
        ready_rx.recv().context("camera thread exited")??;
        Ok(Owner {
            stop: Some(stop_tx),
            thread: Some(thread),
        })
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct View {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Writes frames into the camera DLL's shared memory (once it exists: the DLL
/// creates it when Windows loads it, e.g. when an app opens the camera).
pub struct WinVcamSink {
    /// `None` while the camera could not be started yet (retried every 5 s).
    owner: Option<Owner>,
    start: StartFn,
    /// Result of a background start attempt in flight.
    pending: Option<std::sync::mpsc::Receiver<anyhow::Result<Owner>>>,
    last_start_try: Option<Instant>,
    view: Option<View>,
    last_try: Option<Instant>,
    status: CameraStatus,
    /// Driver version last seen in the mapping (to log a change once).
    dll_version: Option<u32>,
}

// The mapping pointer is only used from the ticker thread that owns the sink.
unsafe impl Send for WinVcamSink {}

impl WinVcamSink {
    pub fn open(spec: &OutputSpec, status: CameraStatus) -> anyhow::Result<WinVcamSink> {
        Self::open_with(
            spec,
            status,
            is_registered(),
            std::sync::Arc::new(Owner::start),
        )
    }

    /// Not installed is a hard error; a failing start (e.g. Frame Server still
    /// restarting) is not: the sink retries from `push`.
    fn open_with(
        _spec: &OutputSpec,
        status: CameraStatus,
        registered: bool,
        start: StartFn,
    ) -> anyhow::Result<WinVcamSink> {
        anyhow::ensure!(registered, NOT_INSTALLED);
        let mut sink = WinVcamSink {
            owner: None,
            start,
            pending: None,
            last_start_try: Some(Instant::now()),
            view: None,
            last_try: None,
            status,
            dll_version: None,
        };
        match (sink.start)() {
            Ok(o) => sink.owner = Some(o),
            Err(e) => sink.start_failed(&e),
        }
        Ok(sink)
    }

    fn start_failed(&self, e: &anyhow::Error) {
        tracing::warn!("virtual camera start failed, will retry: {e:#}");
        self.status.set(Some(format!("starting (retrying): {e:#}")));
    }

    /// Cheap per-frame check: collect a finished background start attempt, or
    /// launch one when due. Never blocks (the attempt runs on its own thread).
    fn poll_start(&mut self, now: Instant) {
        if self.owner.is_some() {
            return;
        }
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(Ok(o)) => {
                    tracing::info!("virtual camera started");
                    self.owner = Some(o);
                    self.pending = None;
                    self.status.set(None);
                }
                Ok(Err(e)) => {
                    self.pending = None;
                    self.start_failed(&e);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.pending = None,
            }
            return;
        }
        if !retry_due(self.last_start_try, now) {
            return;
        }
        self.last_start_try = Some(now);
        let (tx, rx) = std::sync::mpsc::channel();
        let start = self.start.clone();
        let spawned = std::thread::Builder::new()
            .name("p2c-wincam-retry".into())
            .spawn(move || {
                let _ = tx.send(start());
            });
        if spawned.is_ok() {
            self.pending = Some(rx);
        }
    }

    fn try_map(&mut self) {
        if self.view.is_some()
            || self
                .last_try
                .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.last_try = Some(Instant::now());
        let name = HSTRING::from(p2c_shm::MAPPING_NAME);
        unsafe {
            if let Ok(handle) = OpenFileMappingW(FILE_MAP_WRITE.0, false, PCWSTR(name.as_ptr())) {
                let view = MapViewOfFile(handle, FILE_MAP_WRITE, 0, 0, p2c_shm::MAPPING_SIZE);
                if view.Value.is_null() {
                    let _ = CloseHandle(handle);
                } else {
                    tracing::info!("connected to the Page2Cam camera driver");
                    self.view = Some(View { handle, view });
                }
            }
        }
    }
}

impl WinVcamSink {
    /// Compare the driver's version in the mapping with ours; update the
    /// shared status and log once when it changes.
    fn check_driver(&mut self, base: *const u8) {
        // SAFETY: `base` is the mapped view of MAPPING_SIZE bytes.
        let v = unsafe { p2c_shm::dll_version(base) };
        if self.dll_version == Some(v) {
            return;
        }
        self.dll_version = Some(v);
        let status = camera_driver_status(v);
        match &status {
            Some(s) => tracing::warn!(
                dll_version = v,
                app_version = p2c_shm::VERSION,
                "virtual camera: {s}"
            ),
            None => tracing::info!(dll_version = v, "camera driver version ok"),
        }
        self.status.set(status);
    }
}

impl FrameSink for WinVcamSink {
    /// Writes `out.camera` (NV12, fitted into 1920x1080) into the shared memory.
    fn push(&mut self, out: &Output) -> anyhow::Result<()> {
        self.poll_start(Instant::now());
        if self.owner.is_none() {
            return Ok(());
        }
        self.try_map();
        let Some(base) = self.view.as_ref().map(|v| v.view.Value as *mut u8) else {
            return Ok(());
        };
        self.check_driver(base);
        let Some(f) = out.camera.as_deref() else {
            return Ok(());
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        // SAFETY: the view is MAPPING_SIZE bytes, page-aligned, and stays mapped while self lives.
        let ok = unsafe {
            p2c_shm::write_frame(
                base,
                p2c_shm::PixelFormat::Nv12,
                f.width,
                f.height,
                &f.data,
                now,
            )
        };
        anyhow::ensure!(
            ok,
            "camera frame {}x{} does not fit the shared memory",
            f.width,
            f.height
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn spec() -> OutputSpec {
        OutputSpec {
            width: 1280,
            height: 720,
            fps: 30,
            format: p2c_compositor::OutputFormat::Nv12,
        }
    }

    fn failing() -> StartFn {
        Arc::new(|| anyhow::bail!("The data is invalid. (0x8007000D)"))
    }

    #[test]
    fn retry_due_every_5s() {
        let t0 = Instant::now();
        assert!(retry_due(None, t0));
        assert!(!retry_due(Some(t0), t0 + Duration::from_millis(4999)));
        assert!(retry_due(Some(t0), t0 + Duration::from_secs(5)));
    }

    #[test]
    fn not_installed_is_not_retried() {
        let err = WinVcamSink::open_with(&spec(), CameraStatus::default(), false, failing())
            .err()
            .expect("hard error");
        assert!(err.to_string().contains("not installed"), "{err}");
    }

    #[test]
    fn start_failure_reports_retrying_status() {
        let status = CameraStatus::default();
        let sink = WinVcamSink::open_with(&spec(), status.clone(), true, failing())
            .expect("sink despite start failure");
        assert!(sink.owner.is_none());
        let s = status.status().expect("status set");
        assert!(s.starts_with("starting (retrying)"), "{s}");
        assert!(s.contains("0x8007000D"), "{s}");
    }

    #[test]
    fn retry_succeeds_in_background_and_clears_status() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let start: StartFn = Arc::new(move || {
            if c.fetch_add(1, Ordering::SeqCst) == 0 {
                anyhow::bail!("not yet")
            }
            Ok(Owner::dummy())
        });
        let status = CameraStatus::default();
        let mut sink = WinVcamSink::open_with(&spec(), status.clone(), true, start).unwrap();
        let t0 = Instant::now();
        sink.poll_start(t0 + Duration::from_secs(1)); // not due: no attempt
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let due = t0 + Duration::from_secs(6);
        for _ in 0..200 {
            sink.poll_start(due);
            if sink.owner.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(sink.owner.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(status.status(), None);
    }
}
