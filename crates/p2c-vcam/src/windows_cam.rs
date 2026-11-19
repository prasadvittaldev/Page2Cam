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

use crate::{camera_driver_status, CameraMode, CameraStatus};

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

/// KSCATEGORY_VIDEO_CAMERA (ks.h), the interface class camera apps enumerate.
const KSCATEGORY_VIDEO_CAMERA: windows::core::GUID =
    windows::core::GUID::from_u128(0xe5323777_f976_4f5b_9b55_b94699c46e44);

/// Present camera interfaces (path, friendly name) from Plug and Play.
/// Read-only and does not start Frame Server.
fn camera_interfaces() -> anyhow::Result<Vec<(String, Option<String>)>> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_List_SizeW,
        CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CR_BUFFER_SMALL, CR_SUCCESS,
    };
    // The list can grow between the size query and the read: retry a few times.
    for _ in 0..5 {
        let mut len = 0u32;
        let cr = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut len,
                &KSCATEGORY_VIDEO_CAMERA,
                PCWSTR::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        anyhow::ensure!(
            cr == CR_SUCCESS,
            "CM_Get_Device_Interface_List_SizeW: {}",
            cr.0
        );
        let mut buf = vec![0u16; len.max(1) as usize];
        let cr = unsafe {
            CM_Get_Device_Interface_ListW(
                &KSCATEGORY_VIDEO_CAMERA,
                PCWSTR::null(),
                &mut buf,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr == CR_BUFFER_SMALL {
            continue;
        }
        anyhow::ensure!(cr == CR_SUCCESS, "CM_Get_Device_Interface_ListW: {}", cr.0);
        return Ok(buf
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(|s| {
                let path = String::from_utf16_lossy(s);
                let name = interface_name(&path);
                (path, name)
            })
            .collect());
    }
    anyhow::bail!("camera interface list kept changing")
}

/// The interface's friendly name, else its device's name.
fn interface_name(path: &str) -> Option<String> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_Device_Interface_PropertyW, CR_SUCCESS,
    };
    use windows::Win32::Devices::Properties::{
        DEVPKEY_DeviceInterface_FriendlyName, DEVPKEY_NAME, DEVPROPTYPE, DEVPROP_TYPE_STRING,
    };
    let wpath = HSTRING::from(path);
    for key in [DEVPKEY_DeviceInterface_FriendlyName, DEVPKEY_NAME] {
        let mut buf = [0u16; 256];
        let mut size = std::mem::size_of_val(&buf) as u32;
        let mut ty = DEVPROPTYPE(0);
        let cr = unsafe {
            CM_Get_Device_Interface_PropertyW(
                &wpath,
                &key,
                &mut ty,
                Some(buf.as_mut_ptr() as *mut u8),
                &mut size,
                0,
            )
        };
        if cr == CR_SUCCESS && ty == DEVPROP_TYPE_STRING {
            let n = (size as usize / 2).min(buf.len());
            let s = String::from_utf16_lossy(&buf[..n]);
            let s = s.trim_end_matches('\0').to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

/// Is a "Page2Cam" camera device present? `None` when PnP can't be asked.
fn our_camera_present() -> Option<bool> {
    match camera_interfaces() {
        Ok(list) => Some(list.iter().any(|(_, n)| {
            n.as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(p2c_shm::CAMERA_NAME))
        })),
        Err(e) => {
            tracing::warn!("cannot list camera devices: {e:#}");
            None
        }
    }
}

/// How the sink asks whether our camera device still exists (replaced in tests).
type PresentFn = std::sync::Arc<dyn Fn() -> Option<bool> + Send + Sync>;

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
        // Logged so a real run shows whether the device check (used after a
        // Frame Server restart) finds our camera.
        tracing::info!(
            ?service_pid,
            device_present = ?our_camera_present(),
            "virtual camera registered with Frame Server"
        );
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

/// What keeps a mapped block alive.
enum Backing {
    Os {
        handle: HANDLE,
        view: MEMORY_MAPPED_VIEW_ADDRESS,
    },
    /// Heap memory standing in for the mapping (tests).
    #[cfg(test)]
    #[allow(dead_code)]
    Heap(Vec<u64>),
}

impl Drop for Backing {
    fn drop(&mut self) {
        match self {
            Backing::Os { handle, view } => unsafe {
                let _ = UnmapViewOfFile(*view);
                let _ = CloseHandle(*handle);
            },
            #[cfg(test)]
            Backing::Heap(_) => {}
        }
    }
}

/// A writable view of the camera DLL's shared memory (the DLL creates it when
/// Windows loads it, e.g. when an app opens the camera). Used by the engine's
/// sink and by the tray (standby pictures, writer handover); it does not own
/// the camera registration.
pub struct FrameWriter {
    base: *mut u8,
    _backing: Backing,
    /// Local view of a lock held by another writer (stale-lock reclaim).
    stale: std::cell::RefCell<p2c_shm::StaleState>,
}

// The mapping is plain shared memory; the seqlock protocol makes a single
// writer thread safe, and callers keep one writer thread at a time.
unsafe impl Send for FrameWriter {}

impl FrameWriter {
    /// Open the mapping if the DLL has created it yet (one attempt, no retry timer).
    pub fn open() -> Option<FrameWriter> {
        let name = HSTRING::from(p2c_shm::MAPPING_NAME);
        unsafe {
            let handle = OpenFileMappingW(FILE_MAP_WRITE.0, false, PCWSTR(name.as_ptr())).ok()?;
            let view = MapViewOfFile(handle, FILE_MAP_WRITE, 0, 0, p2c_shm::MAPPING_SIZE);
            if view.Value.is_null() {
                let _ = CloseHandle(handle);
                return None;
            }
            Some(FrameWriter {
                base: view.Value as *mut u8,
                _backing: Backing::Os { handle, view },
                stale: Default::default(),
            })
        }
    }

    #[cfg(test)]
    fn from_heap() -> FrameWriter {
        let mut mem = vec![0u64; p2c_shm::MAPPING_SIZE / 8 + 1];
        FrameWriter {
            base: mem.as_mut_ptr() as *mut u8,
            _backing: Backing::Heap(mem),
            stale: Default::default(),
        }
    }

    /// Start of the mapping (header), for `p2c_shm::{writer, set_writer, dll_version}`.
    pub fn base(&self) -> *mut u8 {
        self.base
    }

    /// Write one NV12 frame; false if it doesn't fit the mapping.
    pub fn write_nv12(&self, f: &p2c_core::Nv12Frame, time_ms: u64) -> bool {
        // SAFETY: `base` is MAPPING_SIZE bytes, 8-byte aligned, mapped while self lives.
        unsafe {
            p2c_shm::write_frame(
                self.base,
                p2c_shm::PixelFormat::Nv12,
                f.width,
                f.height,
                &f.data,
                time_ms,
                &mut self.stale.borrow_mut(),
            )
        }
    }

    /// Write one packed BGRA frame; false if it doesn't fit the mapping.
    pub fn write_bgra(&self, width: u32, height: u32, data: &[u8], time_ms: u64) -> bool {
        // SAFETY: as in `write_nv12`.
        unsafe {
            p2c_shm::write_frame(
                self.base,
                p2c_shm::PixelFormat::Bgra,
                width,
                height,
                data,
                time_ms,
                &mut self.stale.borrow_mut(),
            )
        }
    }
}

/// Milliseconds since the Unix epoch (the frame time stamp in the mapping).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Status while the camera is registered again after Frame Server restarted.
const SERVICE_RESTARTED: &str = "starting (retrying): camera service restarted";

/// The "Page2Cam" camera registration (`MFCreateVirtualCamera` on an MTA
/// thread) with its start retry (every 5 s, in the background) and Frame
/// Server recovery (registered again when the service process that held it
/// was replaced and our device is gone). Used by the tray and by the
/// standalone sink. Dropping it removes the camera.
pub struct CameraOwner {
    /// `None` while the camera could not be started yet (retried every 5 s).
    owner: Option<Owner>,
    start: StartFn,
    /// Result of a background start attempt in flight.
    pending: Option<std::sync::mpsc::Receiver<anyhow::Result<Owner>>>,
    last_start_try: Option<Instant>,
    status: CameraStatus,
    /// Frame Server samples (every 5 s, from `spawn_service_watch`).
    watch: std::sync::mpsc::Receiver<Observation>,
    /// When the current owner was installed (older samples are ignored).
    owner_since: Instant,
    /// Frame Server's process id that holds our current registration.
    service_pid: Option<u32>,
    /// Asked after a service restart: is our camera device still there?
    present: PresentFn,
}

impl CameraOwner {
    /// Register the camera now (blocks for the first attempt, usually well
    /// under a second). Not installed is a hard error; a failing start (e.g.
    /// Frame Server still restarting) is not: [`CameraOwner::poll`] retries.
    /// Problems are reported through `status`.
    pub fn open(status: CameraStatus) -> anyhow::Result<CameraOwner> {
        let registered = is_registered();
        Self::open_with(
            status,
            registered,
            std::sync::Arc::new(Owner::start),
            if registered {
                spawn_service_watch()
            } else {
                std::sync::mpsc::channel().1
            },
            std::sync::Arc::new(our_camera_present),
        )
    }

    fn open_with(
        status: CameraStatus,
        registered: bool,
        start: StartFn,
        watch: std::sync::mpsc::Receiver<Observation>,
        present: PresentFn,
    ) -> anyhow::Result<CameraOwner> {
        anyhow::ensure!(registered, NOT_INSTALLED);
        let mut cam = CameraOwner {
            owner: None,
            start,
            pending: None,
            last_start_try: Some(Instant::now()),
            status,
            watch,
            owner_since: Instant::now(),
            service_pid: None,
            present,
        };
        match (cam.start)() {
            Ok(o) => cam.set_owner(o),
            Err(e) => cam.start_failed(&e),
        }
        Ok(cam)
    }

    /// The camera is registered right now.
    pub fn is_registered(&self) -> bool {
        self.owner.is_some()
    }

    /// Cheap, never blocks; call often (each frame, or each second). Collects
    /// a finished background start, launches one when due, and registers the
    /// camera again after a Frame Server restart. True when the registration
    /// was dropped by this call (a writer should reopen the mapping).
    pub fn poll(&mut self, now: Instant) -> bool {
        let lost = self.poll_health();
        self.poll_start(now);
        lost
    }

    /// Drop the registration and register the camera again, in the background
    /// (tray: Force restart). A start already in flight is kept.
    pub fn re_register(&mut self) {
        if self.pending.is_some() {
            return;
        }
        let old = self.owner.take();
        self.service_pid = None;
        self.spawn_start(old, Instant::now());
    }

    fn set_owner(&mut self, o: Owner) {
        self.service_pid = o.service_pid;
        self.owner_since = Instant::now();
        self.owner = Some(o);
    }

    /// Cheap check of the Frame Server samples. When the process that held
    /// our camera was replaced, ask Plug and Play whether our camera device
    /// is still there (it can be, e.g. after an idle stop and a new start by
    /// an app that is opening our camera right now: rebuilding then would cut
    /// that app off). Only a missing device (or one PnP can't confirm) drops
    /// the owner and registers the camera again. True when it was dropped.
    fn poll_health(&mut self) -> bool {
        let mut lost = None;
        while let Ok((at, state)) = self.watch.try_recv() {
            if self.owner.is_none() || at < self.owner_since {
                continue;
            }
            if service_restarted(self.service_pid, state) {
                let present = (self.present)();
                if present == Some(true) {
                    tracing::info!(
                        old_pid = ?self.service_pid,
                        now = ?state,
                        "camera service (Frame Server) restarted; our virtual camera is still present, keeping it"
                    );
                    if let ServiceState::Running { pid } = state {
                        self.service_pid = Some(pid);
                    }
                    continue;
                }
                tracing::info!(
                    old_pid = ?self.service_pid,
                    now = ?state,
                    ?present,
                    "camera service (Frame Server) restarted; our virtual camera is gone, registering it again"
                );
                lost = Some(state);
                break;
            }
            if let (None, ServiceState::Running { pid }) = (self.service_pid, state) {
                self.service_pid = Some(pid);
            }
        }
        if lost.is_none() {
            return false;
        }
        let old = self.owner.take();
        self.service_pid = None;
        self.status.set(Some(SERVICE_RESTARTED.into()));
        self.spawn_start(old, Instant::now());
        true
    }

    /// Start the camera on a short-lived thread ("p2c-wincam-retry"), after
    /// removing `old` (waiting at most 3 s for that, so a hung removal can't
    /// stop the retries).
    fn spawn_start(&mut self, old: Option<Owner>, now: Instant) {
        self.last_start_try = Some(now);
        let (tx, rx) = std::sync::mpsc::channel();
        let start = self.start.clone();
        // Shared so a failed spawn hands `old` back instead of dropping it
        // here (Owner::drop joins a thread that may hang in Remove()).
        let old = std::sync::Arc::new(std::sync::Mutex::new(old));
        let old2 = old.clone();
        let spawned = std::thread::Builder::new()
            .name("p2c-wincam-retry".into())
            .spawn(move || {
                let old = old2.lock().ok().and_then(|mut g| g.take());
                if let Some(o) = old {
                    use std::sync::atomic::{AtomicU8, Ordering};
                    // 0 = removing, 1 = removed, 2 = gave up waiting (new camera started).
                    let removal = std::sync::Arc::new(AtomicU8::new(0));
                    let removal2 = removal.clone();
                    let begun = Instant::now();
                    let (dtx, drx) = std::sync::mpsc::channel::<()>();
                    let _ = std::thread::Builder::new()
                        .name("p2c-wincam-remove".into())
                        .spawn(move || {
                            drop(o);
                            if removal2.swap(1, Ordering::AcqRel) == 2 {
                                // A new camera with the same name/CLSID already runs.
                                tracing::warn!(
                                    took_ms = begun.elapsed().as_millis() as u64,
                                    "the hung removal of the old virtual camera finished late, after a new one was started"
                                );
                            }
                            drop(dtx);
                        });
                    if drx
                        .recv_timeout(Duration::from_secs(3))
                        .is_err_and(|e| e == std::sync::mpsc::RecvTimeoutError::Timeout)
                        && removal
                            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
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
            Err(e) => {
                tracing::warn!("cannot start a virtual camera attempt: {e}");
                if let Some(o) = old.lock().ok().and_then(|mut g| g.take()) {
                    tracing::warn!("leaking the old virtual camera rather than blocking the caller on its removal");
                    std::mem::forget(o);
                }
            }
        }
    }

    fn start_failed(&self, e: &anyhow::Error) {
        tracing::warn!("virtual camera start failed, will retry: {e:#}");
        self.status.set(Some(format!("starting (retrying): {e:#}")));
    }

    /// Collect a finished background start attempt, or launch one when due.
    /// Never blocks (the attempt runs on its own thread).
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
}

/// Writes frames into the camera DLL's shared memory (once it exists: the DLL
/// creates it when Windows loads it, e.g. when an app opens the camera).
pub struct WinVcamSink {
    mode: CameraMode,
    /// The registration (`CameraMode::Own` only; the tray owns it otherwise).
    cam: Option<CameraOwner>,
    writer: Option<FrameWriter>,
    last_try: Option<Instant>,
    status: CameraStatus,
    /// Driver version last seen in the mapping (to log a change once).
    dll_version: Option<u32>,
}

// The mapping pointer is only used from the ticker thread that owns the sink.
unsafe impl Send for WinVcamSink {}

impl WinVcamSink {
    /// `CameraMode::Own` opens the camera registration (today's behaviour);
    /// `FromTray` only connects to the mapping, the tray owns the camera.
    pub fn open(
        _spec: &OutputSpec,
        status: CameraStatus,
        mode: CameraMode,
    ) -> anyhow::Result<WinVcamSink> {
        match mode {
            CameraMode::Own => Ok(Self::with_owner(CameraOwner::open(status.clone())?, status)),
            CameraMode::FromTray => Ok(Self::from_tray(status)),
        }
    }

    fn with_owner(cam: CameraOwner, status: CameraStatus) -> WinVcamSink {
        WinVcamSink {
            mode: CameraMode::Own,
            cam: Some(cam),
            writer: None,
            last_try: None,
            status,
            dll_version: None,
        }
    }

    /// Engine sink for a camera the tray owns: no registration, no start
    /// retry, no Frame Server watch; writes only while `writer == Engine`.
    fn from_tray(status: CameraStatus) -> WinVcamSink {
        WinVcamSink {
            mode: CameraMode::FromTray,
            cam: None,
            writer: None,
            last_try: None,
            status,
            dll_version: None,
        }
    }

    /// Own mode: keep the registration alive; false while there is none.
    fn poll_camera(&mut self, now: Instant) -> bool {
        let Some(cam) = self.cam.as_mut() else {
            return true;
        };
        if cam.poll(now) {
            self.writer = None;
            self.last_try = None;
            self.dll_version = None;
        }
        cam.is_registered()
    }

    fn try_map(&mut self) {
        if self.writer.is_some()
            || self
                .last_try
                .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.last_try = Some(Instant::now());
        if let Some(w) = FrameWriter::open() {
            tracing::info!("connected to the Page2Cam camera driver");
            self.writer = Some(w);
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
        if self.mode == CameraMode::Own && !self.poll_camera(Instant::now()) {
            return Ok(());
        }
        self.try_map();
        let Some(base) = self.writer.as_ref().map(|w| w.base()) else {
            return Ok(());
        };
        self.check_driver(base);
        let Some(writer) = self.writer.as_ref() else {
            return Ok(());
        };
        // The tray owns the camera and the picture until it hands writing over.
        if self.mode == CameraMode::FromTray
            // SAFETY: `base` is the mapped view of MAPPING_SIZE bytes.
            && unsafe { p2c_shm::writer(base) } != p2c_shm::Writer::Engine
        {
            return Ok(());
        }
        let Some(f) = out.camera.as_deref() else {
            return Ok(());
        };
        let ok = writer.write_nv12(f, now_ms());
        // `false` is also "another writer is mid-write": skip that frame
        // quietly; only a frame that can never fit is an error.
        let fits = f.width > 0
            && f.height > 0
            && f.width <= p2c_shm::MAX_WIDTH
            && f.height <= p2c_shm::MAX_HEIGHT
            && f.width.is_multiple_of(2)
            && f.height.is_multiple_of(2);
        anyhow::ensure!(
            ok || fits,
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

    fn failing() -> StartFn {
        Arc::new(|| anyhow::bail!("The data is invalid. (0x8007000D)"))
    }

    /// A service watch nobody feeds (the sender is dropped).
    fn no_watch() -> std::sync::mpsc::Receiver<Observation> {
        std::sync::mpsc::channel().1
    }

    /// Our camera device is missing (after a service restart: rebuild).
    fn gone() -> PresentFn {
        Arc::new(|| Some(false))
    }

    #[test]
    fn service_restart_keeps_a_camera_that_is_still_present() {
        let calls = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        let present: PresentFn = Arc::new(move || {
            a.fetch_add(1, Ordering::SeqCst);
            Some(true)
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let status = CameraStatus::default();
        let mut sink =
            CameraOwner::open_with(status.clone(), true, counting_ok(&calls), rx, present).unwrap();
        let t = Instant::now() + Duration::from_millis(10);
        tx.send((t, ServiceState::Running { pid: 100 })).unwrap();
        tx.send((t, ServiceState::Running { pid: 200 })).unwrap();
        sink.poll_health();
        assert!(sink.owner.is_some(), "camera kept");
        assert!(sink.pending.is_none(), "no restart attempt");
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(sink.service_pid, Some(200), "new baseline");
        assert_eq!(status.status(), None);
        // The same process again: no new device check.
        tx.send((t, ServiceState::Running { pid: 200 })).unwrap();
        sink.poll_health();
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn service_restart_rebuilds_when_the_device_check_fails() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = std::sync::mpsc::channel();
        let unknown: PresentFn = Arc::new(|| None);
        let mut sink = CameraOwner::open_with(
            CameraStatus::default(),
            true,
            counting_ok(&calls),
            rx,
            unknown,
        )
        .unwrap();
        let t = Instant::now() + Duration::from_millis(10);
        tx.send((t, ServiceState::Running { pid: 100 })).unwrap();
        tx.send((t, ServiceState::Running { pid: 200 })).unwrap();
        sink.poll_health();
        assert!(sink.owner.is_none(), "unconfirmed device: rebuild");
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

    /// Read-only Plug and Play listing (`cargo test -p p2c-vcam -- --ignored`).
    #[test]
    #[ignore]
    fn camera_interfaces_lists_the_real_devices() {
        let list = camera_interfaces().expect("listing cameras");
        for (path, name) in &list {
            eprintln!("camera interface: {name:?} {path}");
        }
        eprintln!("Page2Cam present: {:?}", our_camera_present());
    }

    #[test]
    fn service_restart_drops_owner_and_retries() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = std::sync::mpsc::channel();
        let status = CameraStatus::default();
        let cam =
            CameraOwner::open_with(status.clone(), true, counting_ok(&calls), rx, gone()).unwrap();
        assert!(cam.is_registered());
        let mut sink = WinVcamSink::with_owner(cam, status.clone());
        let t = Instant::now() + Duration::from_millis(10);
        tx.send((t, ServiceState::Running { pid: 100 })).unwrap();
        assert!(sink.poll_camera(Instant::now()), "baseline only");
        sink.writer = Some(FrameWriter::from_heap());
        sink.dll_version = Some(2);
        tx.send((t, ServiceState::Running { pid: 200 })).unwrap();
        // Registration dropped (the retry runs in the background): the
        // mapping is reopened and the driver checked again afterwards.
        let _ = sink.poll_camera(Instant::now());
        assert!(sink.writer.is_none());
        assert_eq!(sink.dll_version, None);
        let cam = sink.cam.as_mut().unwrap();
        assert_eq!(
            status.status().as_deref(),
            Some("starting (retrying): camera service restarted")
        );
        // The start-retry loop runs again in the background.
        for _ in 0..200 {
            cam.poll_start(Instant::now());
            if cam.owner.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(cam.owner.is_some(), "camera started again");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(status.status(), None);
        // The new service process is the new baseline; stale samples from
        // before the restart are ignored.
        tx.send((t, ServiceState::Running { pid: 100 })).unwrap();
        tx.send((Instant::now(), ServiceState::Running { pid: 200 }))
            .unwrap();
        assert!(!cam.poll_health());
        tx.send((Instant::now(), ServiceState::Running { pid: 200 }))
            .unwrap();
        assert!(!cam.poll_health());
        assert!(cam.owner.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn re_register_replaces_the_registration() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut cam = CameraOwner::open_with(
            CameraStatus::default(),
            true,
            counting_ok(&calls),
            no_watch(),
            gone(),
        )
        .unwrap();
        assert!(cam.is_registered());
        cam.re_register();
        assert!(!cam.is_registered(), "old registration dropped at once");
        // A second request while the start is in flight is folded into it.
        cam.re_register();
        for _ in 0..200 {
            cam.poll(Instant::now());
            if cam.is_registered() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(cam.is_registered());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn stopped_service_does_not_restart_the_camera() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = std::sync::mpsc::channel();
        let status = CameraStatus::default();
        let mut sink =
            CameraOwner::open_with(status.clone(), true, counting_ok(&calls), rx, gone()).unwrap();
        let t = Instant::now() + Duration::from_millis(10);
        tx.send((t, ServiceState::Running { pid: 100 })).unwrap();
        tx.send((t, ServiceState::Stopped)).unwrap();
        tx.send((t, ServiceState::Running { pid: 100 })).unwrap();
        sink.poll_health();
        assert!(sink.owner.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(status.status(), None);
    }

    fn nv12_out(w: u32, h: u32, fill: u8) -> Output {
        Output {
            camera: Some(Arc::new(p2c_core::Nv12Frame {
                width: w,
                height: h,
                data: vec![fill; (w * h * 3 / 2) as usize],
            })),
            ..Output::default()
        }
    }

    fn mapped_frame(w: &FrameWriter) -> Result<p2c_shm::FrameInfo, p2c_shm::ReadError> {
        unsafe { p2c_shm::read_frame(w.base(), &mut Vec::new()) }
    }

    #[test]
    fn engine_writes_only_when_handed_over() {
        let mut sink = WinVcamSink::from_tray(CameraStatus::default());
        assert!(sink.cam.is_none(), "no camera registration in tray mode");
        sink.writer = Some(FrameWriter::from_heap());
        let base = sink.writer.as_ref().unwrap().base();
        // Nobody / the tray holds the pen: the engine must not touch frames.
        for w in [p2c_shm::Writer::None, p2c_shm::Writer::Tray] {
            unsafe { p2c_shm::set_writer(base, w) };
            sink.push(&nv12_out(4, 2, 9)).unwrap();
            assert_eq!(
                mapped_frame(sink.writer.as_ref().unwrap()),
                Err(p2c_shm::ReadError::NoFrame),
                "{w:?}"
            );
        }
        // Handed over: frames are written.
        unsafe { p2c_shm::set_writer(base, p2c_shm::Writer::Engine) };
        sink.push(&nv12_out(4, 2, 9)).unwrap();
        let info = mapped_frame(sink.writer.as_ref().unwrap()).unwrap();
        assert_eq!((info.width, info.height), (4, 2));
        assert_eq!(info.format, p2c_shm::PixelFormat::Nv12);
        // Taken back: it stops again.
        let seq = info.seq;
        unsafe { p2c_shm::set_writer(base, p2c_shm::Writer::Tray) };
        sink.push(&nv12_out(4, 2, 9)).unwrap();
        assert_eq!(
            mapped_frame(sink.writer.as_ref().unwrap()).unwrap().seq,
            seq
        );
    }

    #[test]
    fn frame_writer_writes_nv12_and_bgra() {
        let w = FrameWriter::from_heap();
        assert!(w.write_nv12(
            &p2c_core::Nv12Frame {
                width: 4,
                height: 2,
                data: vec![1; 12],
            },
            7
        ));
        assert_eq!(mapped_frame(&w).unwrap().format, p2c_shm::PixelFormat::Nv12);
        assert!(w.write_bgra(2, 2, &[3; 16], 8));
        let info = mapped_frame(&w).unwrap();
        assert_eq!(info.format, p2c_shm::PixelFormat::Bgra);
        assert_eq!(info.time_ms, 8);
        // Too big for the mapping.
        assert!(!w.write_nv12(
            &p2c_core::Nv12Frame {
                width: 4000,
                height: 2,
                data: vec![0; 12000],
            },
            1
        ));
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
        let err = CameraOwner::open_with(
            CameraStatus::default(),
            false,
            failing(),
            no_watch(),
            gone(),
        )
        .err()
        .expect("hard error");
        assert!(err.to_string().contains("not installed"), "{err}");
    }

    #[test]
    fn start_failure_reports_retrying_status() {
        let status = CameraStatus::default();
        let sink = CameraOwner::open_with(status.clone(), true, failing(), no_watch(), gone())
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
            CameraOwner::open_with(status.clone(), true, start, no_watch(), gone()).unwrap();
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
