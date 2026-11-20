use std::path::{Path, PathBuf};
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
use p2c_core::{
    apply_crop, Crop, Frame, FrameSlot, Nv12Capture, Nv12Frame, Nv12Gate, Nv12Slot, SharedCrop,
};

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
    /// the camera failed and must be reopened, unless it wraps [`Permanent`].
    fn next_frame(&mut self) -> anyhow::Result<Option<Frame>>;

    /// The NV12 original (uncropped) of the frame `next_frame` just returned,
    /// when the camera delivers NV12. Taken once per frame.
    fn take_nv12(&mut self) -> Option<Nv12Frame> {
        None
    }
}

/// A failure reopening won't fix (e.g. a pixel format we can't decode):
/// capture ends with status "error: ...".
#[derive(Debug)]
struct Permanent(String);

impl std::fmt::Display for Permanent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Permanent {}

/// Warn about a frame failure only the first time, or when the camera had
/// delivered frames since it was last opened; a camera that reopens but keeps
/// failing at once would otherwise warn every 3 s.
fn warn_on_failure(first_failure: bool, frames_since_open: bool) -> bool {
    first_failure || frames_since_open
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
    mut on_frame: impl FnMut(Frame, Option<Nv12Frame>),
    mut on_status: impl FnMut(String),
) {
    let mut source = Some(first);
    let mut last_try: Option<Instant> = None;
    let mut failed_before = false;
    let mut frames_since_open = false;
    while !stop.load(Ordering::Relaxed) {
        match &mut source {
            Some(s) => match s.next_frame() {
                Ok(Some(f)) => {
                    frames_since_open = true;
                    let nv12 = s.take_nv12();
                    on_frame(f, nv12);
                }
                Ok(None) => {}
                Err(e) if e.downcast_ref::<Permanent>().is_some() => {
                    tracing::warn!(camera = %name, error = %format!("{e:#}"), "webcam unusable; capture stopped");
                    on_status(format!("error: {e:#}"));
                    return;
                }
                Err(e) => {
                    if warn_on_failure(!failed_before, frames_since_open) {
                        tracing::warn!(
                            camera = %name,
                            error = %format!("{e:#}"),
                            "webcam stopped; reopening it until it works"
                        );
                    } else {
                        tracing::debug!(camera = %name, error = %format!("{e:#}"), "reopened webcam failed again");
                    }
                    failed_before = true;
                    frames_since_open = false;
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

/// Capture conversion times (decode and crop of one frame, in microseconds)
/// collected between two "webcam measured" log lines. Cheap to clone; the
/// source pushes, the log tick drains.
#[derive(Clone, Default)]
struct ConvertTimes(Arc<Mutex<Vec<u32>>>);

impl ConvertTimes {
    fn push(&self, d: Duration) {
        if let Ok(mut v) = self.0.lock() {
            // A stalled log tick must not grow this without bound.
            if v.len() < 4096 {
                v.push(d.as_micros().min(u32::MAX as u128) as u32);
            }
        }
    }

    /// Drain the samples and return (p50, p95) in ms, or `None` with none.
    fn take_p50_p95_ms(&self) -> Option<(f64, f64)> {
        let mut v = self.0.lock().map(|mut g| std::mem::take(&mut *g)).ok()?;
        percentiles_ms(&mut v)
    }
}

/// (p50, p95) of `micros` in milliseconds (nearest rank), `None` if empty.
fn percentiles_ms(micros: &mut [u32]) -> Option<(f64, f64)> {
    if micros.is_empty() {
        return None;
    }
    micros.sort_unstable();
    let at = |p: f64| {
        let i = ((micros.len() as f64 * p).ceil() as usize).clamp(1, micros.len()) - 1;
        micros[i] as f64 / 1000.0
    };
    Some((at(0.50), at(0.95)))
}

/// The real camera: nokhwa frames decoded to BGRA, already cropped.
struct NokhwaSource {
    cam: Camera,
    nv12: Option<Nv12Frame>,
    gate: Nv12Gate,
    crop: SharedCrop,
    times: ConvertTimes,
}

impl FrameSource for NokhwaSource {
    fn take_nv12(&mut self) -> Option<Nv12Frame> {
        self.nv12.take()
    }

    fn next_frame(&mut self) -> anyhow::Result<Option<Frame>> {
        self.nv12 = None;
        let frame = self.cam.frame()?;
        let res = frame.resolution();
        let Some(fmt) = raw_format(frame.source_frame_format()) else {
            return Err(Permanent(format!(
                "unsupported webcam pixel format {:?}",
                frame.source_frame_format()
            ))
            .into());
        };
        // One read of the crop for both outputs, so they cover the same area.
        let crop = *self.crop.read();
        let t0 = Instant::now();
        match decode_cropped(
            fmt,
            res.width(),
            res.height(),
            frame.buffer(),
            &crop,
            self.gate.is_on(),
        ) {
            Ok((f, nv12)) => {
                self.times.push(t0.elapsed());
                self.nv12 = nv12;
                Ok(Some(f))
            }
            Err(e) => {
                tracing::debug!(error = %e, "dropping undecodable webcam frame");
                Ok(None)
            }
        }
    }
}

impl Drop for NokhwaSource {
    fn drop(&mut self) {
        let _ = self.cam.stop_stream();
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
    /// `own_device` is Page2Cam's output device (Linux loopback path), never
    /// reopened on reconnect.
    pub fn start(
        cam: &CameraInfo,
        own_device: Option<&Path>,
        slot: FrameSlot,
        nv12_slot: Nv12Slot,
        gate: Nv12Gate,
        crop: SharedCrop,
    ) -> anyhow::Result<WebcamCapture> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let cam = cam.clone();
        let own_device = own_device.map(Path::to_path_buf);
        let status = WebcamStatus::new(&format!("ok: {}", cam.name));
        let status2 = status.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
        let (done_tx, done) = std::sync::mpsc::channel::<()>();
        // nokhwa's Camera is not Send, so it is opened and used entirely on this thread.
        let handle = std::thread::Builder::new()
            .name("p2c-webcam".into())
            .spawn(move || {
                let _done = done_tx; // dropped (disconnecting `done`) when this thread exits
                let times = ConvertTimes::default();
                let camera = match open(&cam) {
                    Ok(c) => {
                        let _ = ready_tx.send(Ok(()));
                        NokhwaSource {
                            cam: c,
                            nv12: None,
                            gate: gate.clone(),
                            crop: crop.clone(),
                            times: times.clone(),
                        }
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
                    || {
                        reopen(&cam, own_device.as_deref())
                            .map(|c| NokhwaSource {
                                cam: c,
                                nv12: None,
                                gate: gate.clone(),
                                crop: crop.clone(),
                                times: times.clone(),
                            })
                    },
                    &stop2,
                    reopen_due,
                    &cam.name,
                    |f, nv12| {
                        publish_frame(f, nv12, &slot, &nv12_slot, &gate);
                        let now = Instant::now();
                        if let Some(fps) = rate.tick(now) {
                            match times.take_p50_p95_ms() {
                                Some((p50, p95)) => tracing::info!(
                                    camera = %cam.name,
                                    "webcam measured {fps:.1} fps; capture conversion p50 {p50:.1} ms, p95 {p95:.1} ms"
                                ),
                                None => tracing::info!(camera = %cam.name, "webcam measured {fps:.1} fps"),
                            }
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

/// Decode the camera buffer to BGRA and cut out `crop` with one read of it.
/// For NV12 and YUYV only the cropped rectangle is converted (`Crop::pixels`,
/// always even); MJPEG decodes in full, then crops. With `want_nv12` and an
/// NV12 camera, the same rectangle of the raw NV12 is returned too (an NV12
/// that can't be cut, such as a short buffer, gives `None`: the BGRA covers it).
fn decode_cropped(
    fmt: crate::decode::RawFormat,
    width: u32,
    height: u32,
    buf: &[u8],
    crop: &Crop,
    want_nv12: bool,
) -> anyhow::Result<(Frame, Option<Nv12Frame>)> {
    use crate::decode::{to_bgra, to_bgra_rect, RawFormat};
    if fmt == RawFormat::Mjpeg {
        return Ok((apply_crop(to_bgra(fmt, width, height, buf)?, crop), None));
    }
    let rect = if crop.is_full() {
        (0, 0, width, height)
    } else {
        crop.pixels(width, height)
    };
    let bgra = to_bgra_rect(fmt, width, height, buf, rect)?;
    let nv12 = (want_nv12 && fmt == RawFormat::Nv12)
        .then(|| nv12_rect(width, height, buf, rect))
        .flatten();
    Ok((bgra, nv12))
}

/// The rectangle `(x, y, w, h)` (even values, inside the frame) of an NV12
/// buffer. `None` for anything else, so it can never read out of bounds.
fn nv12_rect(width: u32, height: u32, buf: &[u8], rect: (u32, u32, u32, u32)) -> Option<Nv12Frame> {
    let (fw, fh) = (width as usize, height as usize);
    let (x, y, w, h) = (
        rect.0 as usize,
        rect.1 as usize,
        rect.2 as usize,
        rect.3 as usize,
    );
    if [x, y, w, h, fw, fh].iter().any(|v| v % 2 != 0)
        || w == 0
        || h == 0
        || x + w > fw
        || y + h > fh
        || buf.len() < fw * fh * 3 / 2
    {
        return None;
    }
    let (luma, uv) = buf.split_at(fw * fh);
    let mut data = Vec::with_capacity(w * h * 3 / 2);
    for r in y..y + h {
        data.extend_from_slice(&luma[r * fw + x..r * fw + x + w]);
    }
    for r in y / 2..(y + h) / 2 {
        data.extend_from_slice(&uv[r * fw + x..r * fw + x + w]);
    }
    Nv12Frame::new(rect.2, rect.3, data).ok()
}

/// Publish an already cropped frame and its NV12 twin (when wanted and
/// present). The NV12 goes in first, tagged with the BGRA seq it will get, so
/// a reader that sees BGRA frame N always finds its twin.
fn publish_frame(
    bgra: Frame,
    nv12: Option<Nv12Frame>,
    slot: &FrameSlot,
    nv12_slot: &Nv12Slot,
    gate: &Nv12Gate,
) {
    if let Some(frame) = nv12.filter(|_| gate.is_on()) {
        nv12_slot.put(Nv12Capture {
            bgra_seq: slot.seq() + 1,
            frame,
        });
    }
    slot.put(bgra);
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

/// Reconnect: list the devices again and find the same camera by name (and
/// path), since its enumeration index may now belong to another device, even
/// to our own virtual camera.
fn reopen(cam: &CameraInfo, own_device: Option<&Path>) -> anyhow::Result<Camera> {
    let cams = list_cameras()?;
    let Some(found) = crate::reselect(cam, &cams, own_device) else {
        anyhow::bail!("webcam {} not found", cam.name);
    };
    if found.index != cam.index {
        tracing::info!(camera = %cam.name, old = cam.index, new = found.index, "webcam moved to a new device index");
    }
    open(&found)
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

    fn nv12_data(w: usize, h: usize) -> Vec<u8> {
        // distinct Y per pixel and distinct UV per 2x2 block
        let mut data: Vec<u8> = (0..w * h).map(|i| 30 + (i * 7 % 200) as u8).collect();
        for i in 0..w * h / 2 {
            data.push(if i % 2 == 0 {
                60 + (i * 5 % 120) as u8
            } else {
                100 + (i * 3 % 120) as u8
            });
        }
        data
    }

    fn crops() -> Vec<Crop> {
        vec![
            Crop::FULL,
            Crop {
                x: 0.25,
                y: 0.25,
                w: 0.5,
                h: 0.5,
            },
            // not even in pixels: every path must round the same way
            Crop {
                x: 0.33,
                y: 0.17,
                w: 0.41,
                h: 0.59,
            },
            Crop {
                x: 0.9,
                y: 0.9,
                w: 0.1,
                h: 0.1,
            },
            Crop {
                x: 0.0,
                y: 0.0,
                w: 0.001,
                h: 0.001,
            },
        ]
    }

    #[test]
    fn decode_cropped_equals_decode_then_crop() {
        use crate::decode::{to_bgra, RawFormat};
        let (w, h) = (16usize, 12usize);
        let nv12 = nv12_data(w, h);
        let yuyv: Vec<u8> = (0..w * h * 2).map(|i| (i * 13 % 251) as u8).collect();
        for (fmt, data) in [(RawFormat::Nv12, &nv12), (RawFormat::Yuyv, &yuyv)] {
            let full = to_bgra(fmt, w as u32, h as u32, data).unwrap();
            for crop in crops() {
                let (got, twin) =
                    decode_cropped(fmt, w as u32, h as u32, data, &crop, true).unwrap();
                let want = apply_crop(full.clone(), &crop);
                assert_eq!(got, want, "{fmt:?} {crop:?}");
                // the twin is the same rectangle of the raw NV12
                if fmt == RawFormat::Nv12 {
                    let n = twin.expect("nv12 twin");
                    assert_eq!((n.width, n.height), (got.width, got.height), "{crop:?}");
                    let back = to_bgra(RawFormat::Nv12, n.width, n.height, &n.data).unwrap();
                    assert_eq!(back, got, "{crop:?}");
                } else {
                    assert!(twin.is_none());
                }
            }
        }
    }

    #[test]
    fn mjpeg_still_decodes_in_full_then_crops() {
        use crate::decode::RawFormat;
        let mut jpg = Vec::new();
        let rgb: Vec<u8> = [255u8, 0, 0].repeat(16 * 16);
        jpeg_encoder::Encoder::new(&mut jpg, 95)
            .encode(&rgb, 16, 16, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        let crop = Crop {
            x: 0.25,
            y: 0.25,
            w: 0.5,
            h: 0.5,
        };
        let (f, twin) = decode_cropped(RawFormat::Mjpeg, 16, 16, &jpg, &crop, true).unwrap();
        assert_eq!((f.width, f.height), (8, 8));
        assert!(twin.is_none());
    }

    #[test]
    fn nv12_twin_not_made_unless_wanted_and_bad_input_is_none() {
        use crate::decode::RawFormat;
        let data = nv12_data(8, 8);
        let (_, twin) = decode_cropped(RawFormat::Nv12, 8, 8, &data, &Crop::FULL, false).unwrap();
        assert!(twin.is_none(), "gate off: no copy");
        assert!(nv12_rect(8, 8, &data, (0, 0, 8, 8)).is_some());
        assert!(
            nv12_rect(8, 8, &data[..10], (0, 0, 8, 8)).is_none(),
            "short"
        );
        assert!(nv12_rect(7, 8, &data, (0, 0, 6, 8)).is_none(), "odd width");
        assert!(nv12_rect(8, 8, &data, (6, 0, 4, 4)).is_none(), "outside");
        assert!(nv12_rect(8, 8, &data, (1, 0, 2, 2)).is_none(), "odd x");
    }

    #[test]
    fn percentiles_of_samples() {
        assert_eq!(percentiles_ms(&mut []), None);
        assert_eq!(percentiles_ms(&mut [4000]), Some((4.0, 4.0)));
        let mut v: Vec<u32> = (1..=100).rev().map(|i| i * 100).collect();
        assert_eq!(percentiles_ms(&mut v), Some((5.0, 9.5)));
        let t = ConvertTimes::default();
        t.push(Duration::from_millis(3));
        t.push(Duration::from_millis(7));
        assert_eq!(t.take_p50_p95_ms(), Some((3.0, 7.0)));
        assert_eq!(t.take_p50_p95_ms(), None, "drained");
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
            |_, _| {
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
            |_, _| {},
            |_| {},
        );
        t.join().unwrap();
        assert!(start.elapsed() < Duration::from_millis(600));
    }

    #[test]
    fn unsupported_format_ends_capture_with_error_status() {
        let first = Scripted(
            vec![Err(anyhow::Error::new(Permanent(
                "unsupported webcam pixel format RAWRGB".into(),
            )))]
            .into(),
        );
        let opens = std::sync::atomic::AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let mut statuses = Vec::new();
        capture_loop(
            first,
            || -> anyhow::Result<Scripted> {
                opens.fetch_add(1, Ordering::SeqCst);
                Ok(Scripted(Default::default()))
            },
            &stop,
            |_: Option<Instant>, _: Instant| true,
            "Cam",
            |_, _| {},
            |s| statuses.push(s),
        );
        assert_eq!(opens.load(Ordering::SeqCst), 0, "not reopened");
        assert_eq!(statuses.len(), 1, "{statuses:?}");
        assert!(statuses[0].starts_with("error: "), "{statuses:?}");
        assert!(statuses[0].contains("RAWRGB"), "{statuses:?}");
    }

    #[test]
    fn warns_only_on_first_failure_or_after_frames() {
        assert!(warn_on_failure(true, false), "first failure ever");
        assert!(
            warn_on_failure(false, true),
            "a frame arrived since the last open"
        );
        assert!(
            !warn_on_failure(false, false),
            "reopened camera that never delivered"
        );
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

#[cfg(test)]
mod nv12_gate_tests {
    use super::*;
    use p2c_core::Nv12Gate;

    fn nv12(w: u32, h: u32) -> Nv12Frame {
        Nv12Frame::new(w, h, vec![100; (w * h * 3 / 2) as usize]).unwrap()
    }

    #[test]
    fn nv12_not_published_when_cut_out_is_off() {
        let (slot, nv_slot, gate) = (FrameSlot::new(), Nv12Slot::new(), Nv12Gate::new());
        publish_frame(
            Frame::new_black(8, 8),
            Some(nv12(8, 8)),
            &slot,
            &nv_slot,
            &gate,
        );
        assert_eq!(
            (slot.seq(), nv_slot.seq()),
            (1, 0),
            "BGRA always, NV12 gated"
        );
        gate.set(true);
        publish_frame(
            Frame::new_black(8, 8),
            Some(nv12(8, 8)),
            &slot,
            &nv_slot,
            &gate,
        );
        assert_eq!((slot.seq(), nv_slot.seq()), (2, 1));
        assert_eq!(
            nv_slot.latest().unwrap().bgra_seq,
            2,
            "tagged with its BGRA twin"
        );
    }
}
