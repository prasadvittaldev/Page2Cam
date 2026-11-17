//! Windows 11 virtual camera: registers "Page2Cam" with MFCreateVirtualCamera
//! (our COM media source DLL is then loaded by the Frame Server service) and
//! feeds it frames through the shared memory the DLL creates.
use std::time::{Duration, Instant};

use anyhow::Context;
use p2c_compositor::{scale_frame, FrameSink, OutputSpec};
use p2c_core::Frame;
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

impl Owner {
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
    _owner: Owner,
    view: Option<View>,
    last_try: Option<Instant>,
}

// The mapping pointer is only used from the ticker thread that owns the sink.
unsafe impl Send for WinVcamSink {}

impl WinVcamSink {
    pub fn open(_spec: &OutputSpec) -> anyhow::Result<WinVcamSink> {
        anyhow::ensure!(is_registered(), NOT_INSTALLED);
        let owner = Owner::start()?;
        Ok(WinVcamSink {
            _owner: owner,
            view: None,
            last_try: None,
        })
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

impl FrameSink for WinVcamSink {
    fn push(&mut self, frame: &Frame) -> anyhow::Result<()> {
        self.try_map();
        let Some(v) = &self.view else { return Ok(()) };
        let fits = frame.width <= p2c_shm::MAX_WIDTH && frame.height <= p2c_shm::MAX_HEIGHT;
        let scaled;
        let f = if fits {
            frame
        } else {
            scaled = scale_frame(frame, p2c_shm::MAX_WIDTH, p2c_shm::MAX_HEIGHT);
            &scaled
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        // SAFETY: the view is MAPPING_SIZE bytes, page-aligned, and stays mapped while self lives.
        unsafe { p2c_shm::write_frame(v.view.Value as *mut u8, f.width, f.height, &f.data, now) };
        Ok(())
    }
}
