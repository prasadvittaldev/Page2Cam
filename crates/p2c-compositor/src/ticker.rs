use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use p2c_core::{Frame, FrameSlot};

use parking_lot::RwLock;

use crate::{compose, PipLayout};

const WEBCAM_STALE_AFTER: Duration = Duration::from_secs(1);

/// Tracks when a slot last changed; frames older than 1 s count as absent.
#[derive(Default)]
struct Freshness {
    seq: u64,
    changed: Option<Instant>,
}

impl Freshness {
    fn latest(&mut self, slot: &FrameSlot) -> Option<Arc<Frame>> {
        let seq = slot.seq();
        if seq != self.seq {
            self.seq = seq;
            self.changed = Some(Instant::now());
        }
        self.changed
            .filter(|t| t.elapsed() < WEBCAM_STALE_AFTER)
            .and_then(|_| slot.latest())
    }
}

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
    /// as absent. `matte`: the webcam with the cut-out mask in its alpha byte,
    /// used by cut-out styles (same 1 s staleness). `layout` may be changed live.
    pub fn spawn(
        spec: OutputSpec,
        page: FrameSlot,
        webcam: Option<FrameSlot>,
        matte: Option<FrameSlot>,
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
                let mut cam_fresh = Freshness::default();
                let mut matte_fresh = Freshness::default();
                while !stop2.load(Ordering::Relaxed) {
                    let latest = page.latest();
                    let mut layout = *layout.read();
                    let cam = webcam.as_ref().and_then(|s| cam_fresh.latest(s));
                    let matted = matte.as_ref().and_then(|s| matte_fresh.latest(s));
                    // Cut-out styles need a fresh matte; without one, show the raw
                    // webcam as a box (never a frozen cut-out).
                    let cam = if layout.style.is_cutout() {
                        match matted {
                            Some(m) => Some(m),
                            None => {
                                layout.style = crate::OverlayStyle::Box;
                                cam
                            }
                        }
                    } else {
                        cam
                    };
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
            None,
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

    const RED: [u8; 4] = [0, 0, 255, 255];
    const BLUE_CLEAR: [u8; 4] = [255, 0, 0, 0];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [255, 0, 0, 255];

    fn small_spec() -> OutputSpec {
        OutputSpec {
            width: 64,
            height: 36,
            fps: 30,
        }
    }

    fn red_page() -> FrameSlot {
        let page = FrameSlot::new();
        page.put(Frame::from_bgra(64, 36, RED.repeat(64 * 36)).unwrap());
        page
    }

    fn cutout_layout() -> Arc<parking_lot::RwLock<crate::PipLayout>> {
        let mut l = crate::PipLayout::preset(crate::Corner::BottomRight, 0.25);
        l.style = crate::OverlayStyle::CutoutSmall;
        Arc::new(parking_lot::RwLock::new(l))
    }

    /// Runs the ticker for `ms`, refreshing `fill` every 30 ms, and returns
    /// the colours seen at (55, 30) after `after_ms`.
    fn run_sampled(
        webcam: Option<FrameSlot>,
        matte: Option<FrameSlot>,
        fill: impl Fn() + Send + 'static,
        ms: u64,
        after_ms: u64,
    ) -> Vec<[u8; 4]> {
        let samples = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let start = std::time::Instant::now();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let feeder = std::thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                fill();
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        let t = Ticker::spawn(
            small_spec(),
            red_page(),
            webcam,
            matte,
            cutout_layout(),
            vec![Box::new(Sampler(samples.clone(), 55, 30))],
        );
        std::thread::sleep(Duration::from_millis(ms));
        t.stop();
        stop.store(true, Ordering::SeqCst);
        feeder.join().unwrap();
        let s = samples.lock();
        s.iter()
            .filter(|(t, _)| t.duration_since(start) > Duration::from_millis(after_ms))
            .map(|(_, px)| *px)
            .collect()
    }

    #[test]
    fn ticker_cutout_uses_matte() {
        let cam = FrameSlot::new();
        let matte = FrameSlot::new();
        let (c, m) = (cam.clone(), matte.clone());
        let seen = run_sampled(
            Some(cam),
            Some(matte),
            move || {
                c.put(Frame::from_bgra(16, 9, GREEN.repeat(16 * 9)).unwrap());
                m.put(Frame::from_bgra(16, 9, BLUE.repeat(16 * 9)).unwrap());
            },
            400,
            200,
        );
        assert!(
            !seen.is_empty() && seen.iter().all(|p| *p == BLUE),
            "{seen:?}"
        );
    }

    #[test]
    fn ticker_cutout_falls_back_to_box_when_matte_stale() {
        // Raw webcam has alpha 0: only an opaque Box draw makes it visible.
        let cam = FrameSlot::new();
        let c = cam.clone();
        let seen = run_sampled(
            Some(cam),
            Some(FrameSlot::new()),
            move || c.put(Frame::from_bgra(16, 9, BLUE_CLEAR.repeat(16 * 9)).unwrap()),
            400,
            200,
        );
        assert!(
            !seen.is_empty() && seen.iter().all(|p| *p == BLUE),
            "{seen:?}"
        );
    }

    #[test]
    fn ticker_cutout_hides_when_both_stale() {
        let cam = FrameSlot::new();
        let matte = FrameSlot::new();
        cam.put(Frame::from_bgra(16, 9, GREEN.repeat(16 * 9)).unwrap());
        matte.put(Frame::from_bgra(16, 9, BLUE.repeat(16 * 9)).unwrap());
        let seen = run_sampled(Some(cam), Some(matte), || {}, 1600, 1400);
        assert!(
            !seen.is_empty() && seen.iter().all(|p| *p == RED),
            "{seen:?}"
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
        let t = Ticker::spawn(SPEC, FrameSlot::new(), None, None, layout(), sinks);
        std::thread::sleep(Duration::from_secs(1));
        t.stop();
        assert!(n.load(Ordering::SeqCst) >= 27);
    }
}
