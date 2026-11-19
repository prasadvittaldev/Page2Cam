use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{
    ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
    Resolution,
};
use nokhwa::Camera;
use p2c_core::{apply_crop, Frame, FrameSlot, SharedCrop};

use crate::CameraInfo;

/// List capture devices on this machine.
pub fn list_cameras() -> anyhow::Result<Vec<CameraInfo>> {
    let cams = nokhwa::query(ApiBackend::Auto).context("listing cameras")?;
    Ok(cams
        .iter()
        .filter_map(|c| {
            let CameraIndex::Index(index) = c.index() else {
                return None;
            };
            // Linux backend describes devices as "Video4Linux Device @ /dev/videoN".
            let path = c
                .description()
                .split_once(" @ ")
                .map(|(_, p)| PathBuf::from(p.trim()));
            Some(CameraInfo {
                index: *index,
                name: c.human_name(),
                path,
            })
        })
        .collect())
}

/// Live webcam status for the admin page ("ok: <name>", "reconnecting: <error>").
/// Cheap to clone; the capture thread updates it.
#[derive(Clone, Default)]
pub struct WebcamStatus(Arc<Mutex<String>>);

impl WebcamStatus {
    pub fn new(s: &str) -> WebcamStatus {
        WebcamStatus(Arc::new(Mutex::new(s.to_string())))
    }

    pub fn get(&self) -> String {
        self.0.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn set(&self, s: String) {
        if let Ok(mut g) = self.0.lock() {
            *g = s;
        }
    }
}

/// Gap between attempts to reopen a failed webcam.
const REOPEN_EVERY: Duration = Duration::from_secs(3);

/// True when no reopen was tried yet, or the last one was at least 3 s ago.
fn reopen_due(last_try: Option<Instant>, now: Instant) -> bool {
    last_try.is_none_or(|t| now.saturating_duration_since(t) >= REOPEN_EVERY)
}

/// Where the capture loop gets frames (the nokhwa camera; fakes in tests).
/// Dropping it closes the camera.
trait FrameSource {
    /// The next frame; `Ok(None)` skips one (e.g. undecodable), `Err` means
    /// the camera failed and must be reopened.
    fn next_frame(&mut self) -> anyhow::Result<Option<Frame>>;
}

/// Read frames until `stop`. When the source fails it is closed and reopened
/// with `open` whenever `reopen_due` says so (every 3 s in the app) until that
/// works (e.g. after Windows' Frame Server service crashed and restarted);
/// `stop` is checked every 50 ms meanwhile. `on_status` gets
/// "reconnecting: <error>" while the camera is down and "ok: <name>" once it
/// is back.
fn capture_loop<S: FrameSource>(
    first: S,
    mut open: impl FnMut() -> anyhow::Result<S>,
    stop: &AtomicBool,
    reopen_due: impl Fn(Option<Instant>, Instant) -> bool,
    name: &str,
    mut on_frame: impl FnMut(Frame),
    mut on_status: impl FnMut(String),
) {
    let mut source = Some(first);
    let mut last_try: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        match &mut source {
            Some(s) => match s.next_frame() {
                Ok(Some(f)) => on_frame(f),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        camera = %name,
                        error = %format!("{e:#}"),
                        "webcam stopped; reopening it until it works"
                    );
                    source = None; // closes the camera
                    last_try = Some(Instant::now());
                    on_status(format!("reconnecting: {e:#}"));
                }
            },
            None => {
                let now = Instant::now();
                if reopen_due(last_try, now) {
                    last_try = Some(now);
                    match open() {
                        Ok(s) => {
                            tracing::info!(camera = %name, "webcam reconnected");
                            source = Some(s);
                            on_status(format!("ok: {name}"));
                        }
                        Err(e) => {
                            tracing::debug!(camera = %name, error = %format!("{e:#}"), "webcam reopen failed");
                            on_status(format!("reconnecting: {e:#}"));
                        }
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

/// The real camera: nokhwa frames decoded to BGRA.
struct NokhwaSource(Camera);

impl FrameSource for NokhwaSource {
    fn next_frame(&mut self) -> anyhow::Result<Option<Frame>> {
        let frame = self.0.frame()?;
        let res = frame.resolution();
        let Some(fmt) = raw_format(frame.source_frame_format()) else {
            anyhow::bail!(
                "unsupported webcam pixel format {:?}",
                frame.source_frame_format()
            );
        };
        match crate::decode::to_bgra(fmt, res.width(), res.height(), frame.buffer()) {
            Ok(f) => Ok(Some(f)),
            Err(e) => {
                tracing::debug!(error = %e, "dropping undecodable webcam frame");
                Ok(None)
            }
        }
    }
}

impl Drop for NokhwaSource {
    fn drop(&mut self) {
        let _ = self.0.stop_stream();
    }
}

/// Captures a webcam on its own thread, writing BGRA frames into a slot.
pub struct WebcamCapture {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    /// Disconnects when the capture thread exits.
    done: Receiver<()>,
    status: WebcamStatus,
}

impl WebcamCapture {
    pub fn start(
        cam: &CameraInfo,
        slot: FrameSlot,
        crop: SharedCrop,
    ) -> anyhow::Result<WebcamCapture> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let cam = cam.clone();
        let status = WebcamStatus::new(&format!("ok: {}", cam.name));
        let status2 = status.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
        let (done_tx, done) = std::sync::mpsc::channel::<()>();
        // nokhwa's Camera is not Send, so it is opened and used entirely on this thread.
        let handle = std::thread::Builder::new()
            .name("p2c-webcam".into())
            .spawn(move || {
                let _done = done_tx; // dropped (disconnecting `done`) when this thread exits
                let camera = match open(&cam) {
                    Ok(c) => {
                        let _ = ready_tx.send(Ok(()));
                        NokhwaSource(c)
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                // First report after 5 s (start-up included), then every 30 s,
                // so a camera slowing down (e.g. dim light) shows in the log.
                let mut rate = p2c_core::RateWindow::new(Duration::from_secs(5), Instant::now());
                let mut first = true;
                capture_loop(
                    camera,
                    || open(&cam).map(NokhwaSource),
                    &stop2,
                    reopen_due,
                    &cam.name,
                    |f| {
                        slot.put(crop_frame(f, &crop));
                        let now = Instant::now();
                        if let Some(fps) = rate.tick(now) {
                            tracing::info!(camera = %cam.name, "webcam measured {fps:.1} fps");
                            if first {
                                rate = p2c_core::RateWindow::new(Duration::from_secs(30), now);
                                first = false;
                            }
                        }
                    },
                    |s| status2.set(s),
                );
            })
            .context("spawning webcam thread")?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(WebcamCapture {
                stop,
                handle,
                done,
                status,
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => anyhow::bail!("webcam thread exited during startup"),
        }
    }

    /// Live status ("ok: <name>" or "reconnecting: <error>").
    pub fn status(&self) -> WebcamStatus {
        self.status.clone()
    }

    /// Ask the capture thread to stop. If it is stuck in a blocking read on a
    /// stalled camera, give up after 500 ms and leave it detached so shutdown
    /// never hangs.
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        if !join_within(self.handle, &self.done, Duration::from_millis(500)) {
            tracing::warn!("webcam thread did not stop in time; detaching it");
        }
    }
}

/// Apply the current crop (the lock is read once per frame).
fn crop_frame(frame: Frame, crop: &SharedCrop) -> Frame {
    let c = *crop.read();
    apply_crop(frame, &c)
}

fn raw_format(f: FrameFormat) -> Option<crate::decode::RawFormat> {
    use crate::decode::RawFormat;
    match f {
        FrameFormat::MJPEG => Some(RawFormat::Mjpeg),
        FrameFormat::YUYV => Some(RawFormat::Yuyv),
        FrameFormat::NV12 => Some(RawFormat::Nv12),
        _ => None,
    }
}

/// Join `handle` if its thread finishes within `timeout` (signalled by `done`
/// disconnecting); otherwise detach it. Returns whether it was joined.
fn join_within(handle: JoinHandle<()>, done: &Receiver<()>, timeout: Duration) -> bool {
    match done.recv_timeout(timeout) {
        Err(RecvTimeoutError::Timeout) => false,
        _ => {
            let _ = handle.join();
            true
        }
    }
}

fn open(cam: &CameraInfo) -> anyhow::Result<Camera> {
    let want = CameraFormat::new(Resolution::new(1280, 720), FrameFormat::MJPEG, 30);
    let format = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(want));
    let mut camera = Camera::new(CameraIndex::Index(cam.index), format)
        .with_context(|| format!("opening webcam {}", cam.name))?;
    // Prefer a mode that really delivers ~30 fps (some webcams default to a
    // slow high-resolution mode). If listing fails, keep the Closest request.
    // On Windows, nokhwa lists only whole-number frame rates (a mode at
    // 30000/1001 is left out) and reports the active rate wrongly, so the log
    // shows the mode list and the measured rate rather than its fps.
    match camera.compatible_camera_formats() {
        Ok(modes) => {
            tracing::info!(camera = %cam.name, "webcam modes: {}", crate::format::describe_modes(&modes));
            match crate::format::choose_format(&modes) {
                Some(m) => {
                    tracing::info!(camera = %cam.name, "webcam mode chosen: {m}");
                    let req = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Exact(m));
                    if let Err(e) = camera.set_camera_requset(req) {
                        tracing::warn!(camera = %cam.name, error = %e, "could not select webcam mode {m}");
                    }
                }
                None => {
                    tracing::warn!(camera = %cam.name, "no usable webcam mode listed; using the camera default")
                }
            }
        }
        Err(e) => tracing::warn!(camera = %cam.name, error = %e, "cannot list webcam modes"),
    }
    camera
        .open_stream()
        .with_context(|| format!("starting webcam {}", cam.name))?;
    let fmt = camera.camera_format();
    tracing::info!(camera = %cam.name, "webcam {}x{} {:?}", fmt.width(), fmt.height(), fmt.format());
    Ok(camera)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_does_not_hang_on_blocked_capture_thread() {
        // Simulates nokhwa blocking forever in frame() on a stalled camera.
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3600));
            drop(done_tx);
        });
        let start = Instant::now();
        assert!(!join_within(handle, &done_rx, Duration::from_millis(200)));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn crop_is_read_per_frame() {
        use p2c_core::{Crop, Frame};
        let shared: SharedCrop = Arc::new(parking_lot::RwLock::new(Crop::FULL));
        let out = crop_frame(Frame::new_black(8, 8), &shared);
        assert_eq!((out.width, out.height), (8, 8));
        *shared.write() = Crop {
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        };
        let out = crop_frame(Frame::new_black(8, 8), &shared);
        assert_eq!((out.width, out.height), (4, 4));
    }

    #[test]
    fn reopen_due_every_3s() {
        let t0 = Instant::now();
        assert!(reopen_due(None, t0));
        assert!(!reopen_due(Some(t0), t0 + Duration::from_millis(2999)));
        assert!(reopen_due(Some(t0), t0 + Duration::from_secs(3)));
    }

    /// Plays back scripted results; `Err` once the script runs out is not
    /// reached in these tests (the loop is stopped first).
    struct Scripted(std::collections::VecDeque<anyhow::Result<Option<Frame>>>);

    impl FrameSource for Scripted {
        fn next_frame(&mut self) -> anyhow::Result<Option<Frame>> {
            self.0
                .pop_front()
                .unwrap_or_else(|| Ok(Some(Frame::new_black(4, 4))))
        }
    }

    #[test]
    fn capture_reconnects_after_frame_failure() {
        use std::sync::atomic::AtomicUsize;
        let first = Scripted(
            vec![
                Ok(Some(Frame::new_black(4, 4))),
                Err(anyhow::anyhow!(
                    "Could not capture frame: An exception occurred in the service when handling the control request. (0x80070428)"
                )),
            ]
            .into(),
        );
        let opens = AtomicUsize::new(0);
        let open = || -> anyhow::Result<Scripted> {
            if opens.fetch_add(1, Ordering::SeqCst) < 2 {
                anyhow::bail!("starting webcam Cam: service stopped")
            }
            Ok(Scripted(Default::default()))
        };
        let stop = AtomicBool::new(false);
        let mut frames = 0;
        let mut statuses = Vec::new();
        capture_loop(
            first,
            open,
            &stop,
            |last: Option<Instant>, now: Instant| {
                last.is_none_or(|t| now - t >= Duration::from_millis(10))
            },
            "Cam",
            |_| {
                frames += 1;
                if frames == 4 {
                    stop.store(true, Ordering::Relaxed);
                }
            },
            |s| statuses.push(s),
        );
        assert_eq!(
            opens.load(Ordering::SeqCst),
            3,
            "two failed reopens, then one that works"
        );
        assert_eq!(frames, 4, "frames resume after reconnecting");
        assert!(statuses[0].starts_with("reconnecting: "), "{statuses:?}");
        assert!(statuses[0].contains("0x80070428"), "{statuses:?}");
        assert!(statuses[1].contains("service stopped"), "{statuses:?}");
        assert_eq!(statuses.last().unwrap(), "ok: Cam", "{statuses:?}");
    }

    #[test]
    fn capture_stops_promptly_while_reconnecting() {
        let first = Scripted(vec![Err(anyhow::anyhow!("gone"))].into());
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            s2.store(true, Ordering::Relaxed);
        });
        let start = Instant::now();
        capture_loop(
            first,
            || -> anyhow::Result<Scripted> { anyhow::bail!("still gone") },
            &stop,
            reopen_due,
            "Cam",
            |_| {},
            |_| {},
        );
        t.join().unwrap();
        assert!(start.elapsed() < Duration::from_millis(600));
    }

    #[test]
    fn status_is_shared() {
        let s = WebcamStatus::new("ok: Cam");
        let s2 = s.clone();
        s2.set("reconnecting: x".into());
        assert_eq!(s.get(), "reconnecting: x");
    }

    #[test]
    fn join_within_joins_finished_thread() {
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || drop(done_tx));
        assert!(join_within(handle, &done_rx, Duration::from_secs(2)));
    }
}
