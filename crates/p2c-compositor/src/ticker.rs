use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use p2c_core::{Frame, FrameSlot};

use parking_lot::RwLock;

use crate::{compose, PipLayout};

const WEBCAM_STALE_AFTER: Duration = Duration::from_secs(1);

/// Output video format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputSpec {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// A destination for composited frames (virtual camera, stream encoder, ...).
pub trait FrameSink: Send {
    fn push(&mut self, frame: &Frame) -> anyhow::Result<()>;
}

/// Runs compositing at a fixed rate on its own thread.
pub struct Ticker {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl Ticker {
    /// `webcam`: optional PIP source; a webcam frame not updated for 1 s is treated
    /// as absent. `layout` may be changed live by other threads.
    pub fn spawn(
        spec: OutputSpec,
        page: FrameSlot,
        webcam: Option<FrameSlot>,
        layout: Arc<RwLock<PipLayout>>,
        mut sinks: Vec<Box<dyn FrameSink>>,
    ) -> Ticker {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = std::thread::Builder::new()
            .name("p2c-ticker".into())
            .spawn(move || {
                let period = Duration::from_secs_f64(1.0 / spec.fps.max(1) as f64);
                let mut out = Frame::new_black(spec.width, spec.height);
                let mut last_warn: Vec<Option<Instant>> = vec![None; sinks.len()];
                let start = Instant::now();
                let mut n: u32 = 0;
                let mut cam_seq = 0u64;
                let mut cam_changed = Instant::now();
                while !stop2.load(Ordering::Relaxed) {
                    let latest = page.latest();
                    let cam = webcam.as_ref().and_then(|slot| {
                        let seq = slot.seq();
                        if seq != cam_seq {
                            cam_seq = seq;
                            cam_changed = Instant::now();
                        }
                        (cam_changed.elapsed() < WEBCAM_STALE_AFTER)
                            .then(|| slot.latest())
                            .flatten()
                    });
                    let layout = *layout.read();
                    compose(
                        latest.as_deref(),
                        cam.as_deref().map(|c| (c, &layout)),
                        &mut out,
                    );
                    for (i, sink) in sinks.iter_mut().enumerate() {
                        if let Err(e) = sink.push(&out) {
                            let due =
                                last_warn[i].is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
                            if due {
                                tracing::warn!(sink = i, error = %e, "frame sink failed");
                                last_warn[i] = Some(Instant::now());
                            }
                        }
                    }
                    // Absolute deadlines avoid drift; if we fell behind, skip missed ticks.
                    n += 1;
                    let now = Instant::now();
                    let mut deadline = start + period * n;
                    if deadline < now {
                        n = (now.duration_since(start).as_secs_f64() / period.as_secs_f64()) as u32
                            + 1;
                        deadline = start + period * n;
                    }
                    std::thread::sleep(deadline - now);
                }
            })
            .expect("spawn ticker thread");
        Ticker { stop, handle }
    }

    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2c_core::{Frame, FrameSlot};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    struct Counter(Arc<AtomicUsize>);
    impl FrameSink for Counter {
        fn push(&mut self, _: &Frame) -> anyhow::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct Failing;
    impl FrameSink for Failing {
        fn push(&mut self, _: &Frame) -> anyhow::Result<()> {
            anyhow::bail!("broken sink")
        }
    }

    fn layout() -> Arc<parking_lot::RwLock<crate::PipLayout>> {
        Arc::new(parking_lot::RwLock::new(crate::PipLayout::default()))
    }

    type Samples = Arc<parking_lot::Mutex<Vec<(std::time::Instant, [u8; 4])>>>;

    /// Records the color at one pixel on every pushed frame.
    struct Sampler(Samples, u32, u32);
    impl FrameSink for Sampler {
        fn push(&mut self, f: &Frame) -> anyhow::Result<()> {
            self.0
                .lock()
                .push((std::time::Instant::now(), f.pixel(self.1, self.2)));
            Ok(())
        }
    }

    #[test]
    fn stale_pip_is_hidden() {
        const RED: [u8; 4] = [0, 0, 255, 255];
        const BLUE: [u8; 4] = [255, 0, 0, 255];
        let spec = OutputSpec {
            width: 64,
            height: 36,
            fps: 30,
        };
        let page = FrameSlot::new();
        page.put(Frame::from_bgra(64, 36, RED.repeat(64 * 36)).unwrap());
        let cam = FrameSlot::new();
        cam.put(Frame::from_bgra(16, 9, BLUE.repeat(16 * 9)).unwrap());
        let layout = Arc::new(parking_lot::RwLock::new(crate::PipLayout::preset(
            crate::Corner::BottomRight,
            0.25,
        )));
        // Bottom-right 16x9 PIP sits at (47, 26); sample its center.
        let samples = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let start = std::time::Instant::now();
        let t = Ticker::spawn(
            spec,
            page,
            Some(cam),
            layout,
            vec![Box::new(Sampler(samples.clone(), 55, 30))],
        );
        std::thread::sleep(Duration::from_millis(1700));
        t.stop();
        let s = samples.lock();
        let early = s
            .iter()
            .any(|(t, px)| t.duration_since(start) < Duration::from_millis(500) && *px == BLUE);
        let late: Vec<_> = s
            .iter()
            .filter(|(t, _)| t.duration_since(start) > Duration::from_millis(1500))
            .collect();
        assert!(early, "PIP never shown");
        assert!(
            !late.is_empty() && late.iter().all(|(_, px)| *px == RED),
            "stale PIP still shown"
        );
    }

    const SPEC: OutputSpec = OutputSpec {
        width: 16,
        height: 16,
        fps: 30,
    };

    #[test]
    fn ticker_runs_at_fps() {
        let n = Arc::new(AtomicUsize::new(0));
        let t = Ticker::spawn(
            SPEC,
            FrameSlot::new(),
            None,
            layout(),
            vec![Box::new(Counter(n.clone()))],
        );
        std::thread::sleep(Duration::from_secs(1));
        t.stop();
        let c = n.load(Ordering::SeqCst);
        assert!((27..=33).contains(&c), "pushed {c} frames");
    }

    #[test]
    fn failing_sink_does_not_starve_others() {
        let n = Arc::new(AtomicUsize::new(0));
        let sinks: Vec<Box<dyn FrameSink>> = vec![Box::new(Failing), Box::new(Counter(n.clone()))];
        let t = Ticker::spawn(SPEC, FrameSlot::new(), None, layout(), sinks);
        std::thread::sleep(Duration::from_secs(1));
        t.stop();
        assert!(n.load(Ordering::SeqCst) >= 27);
    }
}
