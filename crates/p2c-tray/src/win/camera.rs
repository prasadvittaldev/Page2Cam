//! The tray's side of the virtual camera: the registration (`CameraOwner`:
//! start retry, Frame Server recovery, removed on drop), the frame mapping,
//! the shared-memory `writer` field (only the tray sets it) and the standby
//! picture, written about once a second while the tray holds the pen.

use std::time::{Duration, Instant};

use p2c_shm::Writer;
use p2c_vcam::{CameraOwner, CameraStatus, FrameWriter};

use crate::standby::{standby_nv12, StandbyKind};

/// Standby frames per second (the DLL repeats the last frame in between;
/// it shows its own "waiting" pattern only after 2 s without one).
const STANDBY_EVERY: Duration = Duration::from_secs(1);
/// How often a missing mapping is looked for (it exists once an app has
/// opened the camera).
const MAP_EVERY: Duration = Duration::from_secs(1);

pub struct Camera {
    owner: Option<CameraOwner>,
    status: CameraStatus,
    /// Why there is no registration at all (not installed).
    open_error: Option<String>,
    writer: Option<FrameWriter>,
    last_map_try: Option<Instant>,
    /// Who should write frames (applied to every mapping we open).
    desired: Writer,
    /// Standby writes allowed from here on (grace after taking the pen back).
    resume_at: Option<Instant>,
    next_standby: Instant,
    /// Driver problem read from the mapping (out of date).
    driver_problem: Option<String>,
}

// SAFETY: owned by one thread at a time: the controller, and at Quit the
// thread that removes the camera (it is moved there whole). The mapping
// pointer and COM objects inside are never used from two threads at once.
unsafe impl Send for Camera {}

impl Camera {
    /// Register the camera in the background (returns at once). A missing driver
    /// is reported through [`Camera::problem`]; the tray keeps running.
    pub fn open() -> Camera {
        let status = CameraStatus::default();
        let (owner, open_error) = match CameraOwner::open_background(status.clone()) {
            Ok(o) => (Some(o), None),
            Err(e) => {
                tracing::error!("virtual camera: {e:#}");
                (None, Some(format!("{e:#}")))
            }
        };
        Camera {
            owner,
            status,
            open_error,
            writer: None,
            last_map_try: None,
            desired: Writer::Tray,
            resume_at: Some(Instant::now()),
            next_standby: Instant::now(),
            driver_problem: None,
        }
    }

    /// No camera at all (after Quit: dropping the old one removed it).
    pub fn closed() -> Camera {
        Camera {
            owner: None,
            status: CameraStatus::default(),
            open_error: None,
            writer: None,
            last_map_try: None,
            desired: Writer::None,
            resume_at: None,
            next_standby: Instant::now(),
            driver_problem: None,
        }
    }

    /// A red problem for the icon: driver not installed or out of date.
    pub fn problem(&self) -> Option<String> {
        if self.open_error.is_some() {
            return Some("camera driver not installed".into());
        }
        self.driver_problem
            .as_ref()
            .map(|_| "camera driver out of date".to_string())
    }

    /// The camera's status line in the engine's words (for `classify`).
    pub fn status_line(&self) -> String {
        if let Some(e) = &self.open_error {
            return format!("error: {e}");
        }
        if let Some(p) = &self.driver_problem {
            return p.clone();
        }
        self.status
            .status()
            .unwrap_or_else(|| "ok: Windows camera \"Page2Cam\"".into())
    }

    /// Hand the pen to `w` (the field is set right away when mapped). Taking
    /// it back for the tray does not write yet: see [`Camera::resume_at`].
    pub fn set_writer(&mut self, w: Writer) {
        if w != Writer::Tray {
            self.resume_at = None;
        } else if self.desired != Writer::Tray {
            // Taking the pen back: never write before the grace has passed
            // (`ResumeStandbyAfter` sets the exact time).
            self.resume_at = Some(Instant::now() + crate::state::STANDBY_GRACE);
        }
        self.desired = w;
        if let Some(m) = &self.writer {
            // SAFETY: mapped view of MAPPING_SIZE bytes.
            unsafe { p2c_shm::set_writer(m.base(), w) };
        }
        tracing::info!(writer = ?w, "camera writer");
    }

    /// Standby writes may start at `t` (after the take-back grace).
    pub fn resume_at(&mut self, t: Instant) {
        self.resume_at = Some(t);
        self.next_standby = t;
    }

    /// Force restart: register the camera again (in the background).
    pub fn re_register(&mut self) {
        if let Some(o) = &mut self.owner {
            o.re_register();
            self.writer = None;
            self.last_map_try = None;
        } else {
            // Maybe it was installed since.
            let mut fresh = Camera::open();
            fresh.desired = self.desired;
            fresh.resume_at = self.resume_at;
            *self = fresh;
        }
    }

    /// Keep everything going; cheap, call often. Returns when it wants to be
    /// called again at the latest.
    pub fn service(&mut self, now: Instant, kind: StandbyKind) -> Instant {
        if let Some(o) = &mut self.owner {
            if o.poll(now) {
                self.writer = None;
                self.last_map_try = None;
            }
        }
        self.map(now);
        let Some(m) = &self.writer else {
            return now + MAP_EVERY;
        };
        // SAFETY: mapped view of MAPPING_SIZE bytes.
        unsafe {
            // A new mapping (new DLL instance) starts with writer = none.
            if p2c_shm::writer(m.base()) != self.desired {
                p2c_shm::set_writer(m.base(), self.desired);
            }
        }
        let Some(resume) = self.resume_at.filter(|_| self.desired == Writer::Tray) else {
            return now + Duration::from_secs(1);
        };
        if now < resume {
            return resume;
        }
        if now >= self.next_standby {
            m.write_nv12(standby_nv12(kind), p2c_vcam::now_ms());
            self.next_standby = now + STANDBY_EVERY;
        }
        self.next_standby
    }

    fn map(&mut self, now: Instant) {
        if self.writer.is_some()
            || self
                .last_map_try
                .is_some_and(|t| now.saturating_duration_since(t) < MAP_EVERY)
        {
            return;
        }
        self.last_map_try = Some(now);
        let Some(m) = FrameWriter::open() else {
            return;
        };
        tracing::info!("connected to the Page2Cam camera driver");
        // SAFETY: mapped view of MAPPING_SIZE bytes.
        let v = unsafe { p2c_shm::dll_version(m.base()) };
        let problem = p2c_vcam::camera_driver_status(v);
        if problem != self.driver_problem {
            match &problem {
                Some(p) => tracing::warn!(
                    dll_version = v,
                    app_version = p2c_shm::VERSION,
                    "virtual camera: {p}"
                ),
                None => tracing::info!(dll_version = v, "camera driver version ok"),
            }
        }
        self.driver_problem = problem;
        // SAFETY: as above.
        unsafe { p2c_shm::set_writer(m.base(), self.desired) };
        self.writer = Some(m);
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        if let Some(m) = &self.writer {
            // Nobody writes any more; the DLL shows its own pattern.
            // SAFETY: mapped view of MAPPING_SIZE bytes.
            unsafe { p2c_shm::set_writer(m.base(), Writer::None) };
        }
        self.writer = None;
        if self.owner.take().is_some() {
            tracing::info!("virtual camera removed");
        }
    }
}
