use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Context;
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{
    ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
    Resolution,
};
use nokhwa::Camera;
use p2c_core::FrameSlot;

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

/// Captures a webcam on its own thread, writing BGRA frames into a slot.
pub struct WebcamCapture {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    /// Disconnects when the capture thread exits.
    done: Receiver<()>,
}

impl WebcamCapture {
    pub fn start(cam: &CameraInfo, slot: FrameSlot) -> anyhow::Result<WebcamCapture> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let cam = cam.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
        let (done_tx, done) = std::sync::mpsc::channel::<()>();
        // nokhwa's Camera is not Send, so it is opened and used entirely on this thread.
        let handle = std::thread::Builder::new()
            .name("p2c-webcam".into())
            .spawn(move || {
                let _done = done_tx; // dropped (disconnecting `done`) when this thread exits
                let mut camera = match open(&cam) {
                    Ok(c) => {
                        let _ = ready_tx.send(Ok(()));
                        c
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                while !stop2.load(Ordering::Relaxed) {
                    let frame = match camera.frame() {
                        Ok(f) => f,
                        Err(e) => {
                            // Unplugged or failed: stop updating; the PIP hides itself when stale.
                            tracing::warn!(camera = %cam.name, error = %e, "webcam stopped");
                            break;
                        }
                    };
                    let res = frame.resolution();
                    let Some(fmt) = raw_format(frame.source_frame_format()) else {
                        tracing::warn!(format = ?frame.source_frame_format(), "unsupported webcam pixel format");
                        break;
                    };
                    match crate::decode::to_bgra(fmt, res.width(), res.height(), frame.buffer()) {
                        Ok(f) => slot.put(f),
                        Err(e) => tracing::debug!(error = %e, "dropping undecodable webcam frame"),
                    }
                }
                let _ = camera.stop_stream();
            })
            .context("spawning webcam thread")?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(WebcamCapture { stop, handle, done }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => anyhow::bail!("webcam thread exited during startup"),
        }
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
    camera
        .open_stream()
        .with_context(|| format!("starting webcam {}", cam.name))?;
    let fmt = camera.camera_format();
    tracing::info!(camera = %cam.name, "webcam {}x{}@{} {:?}", fmt.width(), fmt.height(), fmt.frame_rate(), fmt.format());
    Ok(camera)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

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
    fn join_within_joins_finished_thread() {
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || drop(done_tx));
        assert!(join_within(handle, &done_rx, Duration::from_secs(2)));
    }
}
