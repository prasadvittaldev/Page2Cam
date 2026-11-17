use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::Context;
use nokhwa::pixel_format::RgbAFormat;
use nokhwa::utils::{
    ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
    Resolution,
};
use nokhwa::Camera;
use p2c_core::{Frame, FrameSlot};

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
}

impl WebcamCapture {
    pub fn start(cam: &CameraInfo, slot: FrameSlot) -> anyhow::Result<WebcamCapture> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let cam = cam.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
        // nokhwa's Camera is not Send, so it is opened and used entirely on this thread.
        let handle = std::thread::Builder::new()
            .name("p2c-webcam".into())
            .spawn(move || {
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
                    let mut data = vec![0u8; res.width() as usize * res.height() as usize * 4];
                    if let Err(e) = frame.decode_image_to_buffer::<RgbAFormat>(&mut data) {
                        tracing::debug!(error = %e, "dropping undecodable webcam frame");
                        continue;
                    }
                    for px in data.as_chunks_mut::<4>().0 {
                        px.swap(0, 2); // RGBA -> BGRA
                    }
                    if let Ok(f) = Frame::from_bgra(res.width(), res.height(), data) {
                        slot.put(f);
                    }
                }
                let _ = camera.stop_stream();
            })
            .context("spawning webcam thread")?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(WebcamCapture { stop, handle }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => anyhow::bail!("webcam thread exited during startup"),
        }
    }

    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

fn open(cam: &CameraInfo) -> anyhow::Result<Camera> {
    let want = CameraFormat::new(Resolution::new(1280, 720), FrameFormat::MJPEG, 30);
    let format = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Closest(want));
    let mut camera = Camera::new(CameraIndex::Index(cam.index), format)
        .with_context(|| format!("opening webcam {}", cam.name))?;
    camera
        .open_stream()
        .with_context(|| format!("starting webcam {}", cam.name))?;
    let fmt = camera.camera_format();
    tracing::info!(camera = %cam.name, "webcam {}x{}@{} {:?}", fmt.width(), fmt.height(), fmt.frame_rate(), fmt.format());
    Ok(camera)
}
