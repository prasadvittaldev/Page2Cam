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
    /// Frame Server's process id right after the camera was registered.
    service_pid: Option<u32>,
}

/// Windows' camera Frame Server service, as the SCM reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceState {
    Running {
        pid: u32,
    },
    Stopped,
    /// Starting or stopping.
    Pending,
    /// The query failed.
    Unknown,
}

/// A service sample and when it was taken.
type Observation = (Instant, ServiceState);

/// How often the Frame Server service is checked.
const HEALTH_EVERY: Duration = Duration::from_secs(5);

/// Our camera lives inside the Frame Server process that registered it
/// (`registered_pid`). Only a *different* running process means that one died
/// (crash, watchdog) and our camera with it. A stopped service alone is no
/// reason: Frame Server is demand-started and stops when idle, and starting
/// the camera again would restart it in a loop.
fn service_restarted(registered_pid: Option<u32>, now: ServiceState) -> bool {
    matches!((registered_pid, now), (Some(p), ServiceState::Running { pid }) if pid != p)
}

/// Read-only SCM query of the "FrameServer" service (no admin rights needed).
fn query_frame_server() -> ServiceState {
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT,
        SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
        SERVICE_STOPPED,
    };
    unsafe {
        let Ok(scm) = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) else {
            return ServiceState::Unknown;
        };
        let state = match OpenServiceW(scm, windows::core::w!("FrameServer"), SERVICE_QUERY_STATUS)
        {
            Ok(svc) => {
                let mut buf = [0u8; std::mem::size_of::<SERVICE_STATUS_PROCESS>()];
                let mut needed = 0u32;
                let r =
                    QueryServiceStatusEx(svc, SC_STATUS_PROCESS_INFO, Some(&mut buf), &mut needed);
                let _ = CloseServiceHandle(svc);
                if r.is_ok() {
                    let s: SERVICE_STATUS_PROCESS =
                        std::ptr::read_unaligned(buf.as_ptr() as *const SERVICE_STATUS_PROCESS);
                    if s.dwCurrentState == SERVICE_RUNNING && s.dwProcessId != 0 {
                        ServiceState::Running { pid: s.dwProcessId }
                    } else if s.dwCurrentState == SERVICE_STOPPED {
                        ServiceState::Stopped
                    } else {
                        ServiceState::Pending
                    }
                } else {
                    ServiceState::Unknown
                }
            }
            Err(_) => ServiceState::Unknown,
        };
        let _ = CloseServiceHandle(scm);
        state
    }
}

/// Sample the Frame Server every 5 s on its own thread (never the ticker's);
/// ends once the sink is gone.
fn spawn_service_watch() -> std::sync::mpsc::Receiver<Observation> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("p2c-wincam-health".into())
        .spawn(move || loop {
            if tx.send((Instant::now(), query_frame_server())).is_err() {
                return;
            }
            std::thread::sleep(HEALTH_EVERY);
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot watch the camera service: {e}");
    }
    rx
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
            service_pid: None,
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
                if let Err(e) = cam.Start(None) {
                    // Don't leave a half-registered camera behind.
                    let _ = cam.Remove();
                    return Err(anyhow::Error::from(e).context(
                        "starting the virtual camera. If this says access denied, re-run install-camera.cmd as administrator",
                    ));
                }
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
        let service_pid = match query_frame_server() {
            ServiceState::Running { pid } => Some(pid),
            _ => None,
        };
        tracing::info!(?service_pid, "virtual camera registered with Frame Server");
        Ok(Owner {
            stop: Some(stop_tx),
            thread: Some(thread),
            service_pid,
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
    /// Frame Server samples (every 5 s, from `spawn_service_watch`).
    watch: std::sync::mpsc::Receiver<Observation>,
    /// When the current owner was installed (older samples are ignored).
    owner_since: Instant,
    /// Frame Server's process id that holds our current registration.
    service_pid: Option<u32>,
}

/// Status while the camera is registered again after Frame Server restarted.
const SERVICE_RESTARTED: &str = "starting (retrying): camera service restarted";

// The mapping pointer is only used from the ticker thread that owns the sink.
unsafe impl Send for WinVcamSink {}

impl WinVcamSink {
    pub fn open(spec: &OutputSpec, status: CameraStatus) -> anyhow::Result<WinVcamSink> {
        let registered = is_registered();
        Self::open_with(
            spec,
            status,
            registered,
            std::sync::Arc::new(Owner::start),
            if registered {
                spawn_service_watch()
            } else {
                std::sync::mpsc::channel().1
            },
        )
    }

    /// Not installed is a hard error; a failing start (e.g. Frame Server still
    /// restarting) is not: the sink retries from `push`.
    fn open_with(
        _spec: &OutputSpec,
        status: CameraStatus,
        registered: bool,
        start: StartFn,
        watch: std::sync::mpsc::Receiver<Observation>,
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
            watch,
            owner_since: Instant::now(),
            service_pid: None,
        };
        match (sink.start)() {
            Ok(o) => sink.set_owner(o),
            Err(e) => sink.start_failed(&e),
        }
        Ok(sink)
    }

    fn set_owner(&mut self, o: Owner) {
        self.service_pid = o.service_pid;
        self.owner_since = Instant::now();
        self.owner = Some(o);
    }

    /// Cheap per-frame check of the Frame Server samples: when the process
    /// that held our camera is gone, drop the owner and register again.
    fn poll_health(&mut self) {
        let mut lost = None;
        while let Ok((at, state)) = self.watch.try_recv() {
            if self.owner.is_none() || at < self.owner_since {
                continue;
            }
            if service_restarted(self.service_pid, state) {
                lost = Some(state);
                break;
            }
            if let (None, ServiceState::Running { pid }) = (self.service_pid, state) {
                self.service_pid = Some(pid);
            }
        }
        if let Some(state) = lost {
            tracing::warn!(
                old_pid = ?self.service_pid,
                now = ?state,
                "camera service (Frame Server) restarted: our virtual camera went with it; registering it again"
            );
            let old = self.owner.take();
            self.view = None;
            self.last_try = None;
            self.dll_version = None;
            self.service_pid = None;
            self.status.set(Some(SERVICE_RESTARTED.into()));
            self.spawn_start(old, Instant::now());
        }
    }

    /// Start the camera on a short-lived thread ("p2c-wincam-retry"), after
    /// removing `old` (waiting at most 3 s for that, so a hung removal can't
    /// stop the retries).
    fn spawn_start(&mut self, old: Option<Owner>, now: Instant) {
        self.last_start_try = Some(now);
        let (tx, rx) = std::sync::mpsc::channel();
        let start = self.start.clone();
        let spawned = std::thread::Builder::new()
            .name("p2c-wincam-retry".into())
            .spawn(move || {
                if let Some(o) = old {
                    let (dtx, drx) = std::sync::mpsc::channel::<()>();
                    let _ = std::thread::Builder::new()
                        .name("p2c-wincam-remove".into())
                        .spawn(move || {
                            drop(o);
                            drop(dtx);
                        });
                    if drx
                        .recv_timeout(Duration::from_secs(3))
                        .is_err_and(|e| e == std::sync::mpsc::RecvTimeoutError::Timeout)
                    {
                        tracing::warn!(
                            "removing the old virtual camera hangs; starting a new one anyway"
                        );
                    }
                }
                let _ = tx.send(start());
            });
        match spawned {
            Ok(_) => self.pending = Some(rx),
            Err(e) => tracing::warn!("cannot start a virtual camera attempt: {e}"),
        }
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
                    self.set_owner(o);
                    self.pending = None;
                    self.status.set(None);
                }
                Ok(Err(e)) => {
                    self.pending = None;
                    self.start_failed(&e);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    tracing::warn!("virtual camera start attempt ended without a result (panicked?); will retry");
                    self.pending = None;
                }
            }
            return;
        }
        if !retry_due(self.last_start_try, now) {
            return;
        }
        self.spawn_start(None, now);
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
        self.poll_health();
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

    /// A service watch nobody feeds (the sender is dropped).
    fn no_watch() -> std::sync::mpsc::Receiver<Observation> {
        std::sync::mpsc::channel().1
    }

    fn counting_ok(calls: &Arc<AtomicUsize>) -> StartFn {
        let c = calls.clone();
        Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(Owner::dummy())
        })
    }

    #[test]
    fn only_a_new_service_process_means_our_camera_is_gone() {
        use ServiceState::*;
        // Frame Server is demand-started: stopping on its own is normal.
        assert!(!service_restarted(Some(100), Stopped));
        assert!(!service_restarted(Some(100), Pending));
        assert!(!service_restarted(Some(100), Unknown));
        assert!(!service_restarted(Some(100), Running { pid: 100 }));
        // A different process: the one that held our registration died.
        assert!(service_restarted(Some(100), Running { pid: 200 }));
        // No baseline yet: nothing to compare with.
        assert!(!service_restarted(None, Running { pid: 200 }));
    }

    /// Read-only query of the real service (`cargo test -p p2c-vcam -- --ignored`).
    #[test]
    #[ignore]
    fn frame_server_query_reads_the_real_service() {
        let s = query_frame_server();
        eprintln!("FrameServer: {s:?}");
        assert!(matches!(
            s,
            ServiceState::Running { .. } | ServiceState::Stopped | ServiceState::Pending
        ));
    }

    #[test]
    fn service_restart_drops_owner_and_retries() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = std::sync::mpsc::channel();
        let status = CameraStatus::default();
        let mut sink =
            WinVcamSink::open_with(&spec(), status.clone(), true, counting_ok(&calls), rx).unwrap();
        assert!(sink.owner.is_some());
        let t = Instant::now() + Duration::from_millis(10);
        tx.send((t, ServiceState::Running { pid: 100 })).unwrap();
        sink.poll_health();
        assert!(sink.owner.is_some(), "baseline only");
        sink.dll_version = Some(2);
        tx.send((t, ServiceState::Running { pid: 200 })).unwrap();
        sink.poll_health();
        assert!(sink.owner.is_none(), "owner dropped");
        assert!(sink.view.is_none());
        assert_eq!(sink.dll_version, None);
        assert_eq!(
            status.status().as_deref(),
            Some("starting (retrying): camera service restarted")
        );
        // The start-retry loop runs again in the background.
        for _ in 0..200 {
            sink.poll_start(Instant::now());
            if sink.owner.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(sink.owner.is_some(), "camera started again");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(status.status(), None);
        // The new service process is the new baseline; stale samples from
        // before the restart are ignored.
        tx.send((t, ServiceState::Running { pid: 100 })).unwrap();
        tx.send((Instant::now(), ServiceState::Running { pid: 200 }))
            .unwrap();
        sink.poll_health();
        tx.send((Instant::now(), ServiceState::Running { pid: 200 }))
            .unwrap();
        sink.poll_health();
        assert!(sink.owner.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn stopped_service_does_not_restart_the_camera() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = std::sync::mpsc::channel();
        let status = CameraStatus::default();
        let mut sink =
            WinVcamSink::open_with(&spec(), status.clone(), true, counting_ok(&calls), rx).unwrap();
        let t = Instant::now() + Duration::from_millis(10);
        tx.send((t, ServiceState::Running { pid: 100 })).unwrap();
        tx.send((t, ServiceState::Stopped)).unwrap();
        tx.send((t, ServiceState::Running { pid: 100 })).unwrap();
        sink.poll_health();
        assert!(sink.owner.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(status.status(), None);
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
        let err = WinVcamSink::open_with(
            &spec(),
            CameraStatus::default(),
            false,
            failing(),
            no_watch(),
        )
        .err()
        .expect("hard error");
        assert!(err.to_string().contains("not installed"), "{err}");
    }

    #[test]
    fn start_failure_reports_retrying_status() {
        let status = CameraStatus::default();
        let sink = WinVcamSink::open_with(&spec(), status.clone(), true, failing(), no_watch())
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
        let mut sink =
            WinVcamSink::open_with(&spec(), status.clone(), true, start, no_watch()).unwrap();
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
