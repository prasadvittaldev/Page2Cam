use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use p2c_core::{
    bgra_to_nv12, camera_size, DurationStats, Frame, FrameSlot, Mask, MaskSlot, Nv12Frame, Slot,
};

use parking_lot::RwLock;

use crate::{Compositor, Pip, PipLayout};

const WEBCAM_STALE_AFTER: Duration = Duration::from_secs(1);
/// How often the ticker's timing is written to the log.
const LOG_EVERY: Duration = Duration::from_secs(30);
/// Admin preview rate the ticker aims at (frames per second).
const PREVIEW_FPS: u32 = 12;

/// Tracks when a slot last changed; values older than 1 s count as absent.
#[derive(Default)]
struct Freshness {
    seq: u64,
    changed: Option<Instant>,
}

impl Freshness {
    fn latest<T>(&mut self, slot: &Slot<T>) -> Option<Arc<T>> {
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

/// Pixel format the sinks are fed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    /// Packed BGRA in `Output::bgra` (Linux: v4l2 YUYV sink, BGRA stream input).
    Bgra,
    /// NV12 (BT.709 limited) in `Output::nv12` and `Output::camera` (Windows).
    Nv12,
}

/// Output video format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputSpec {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub format: OutputFormat,
}

/// One output frame in the forms the sinks need. Unused forms are `None`.
#[derive(Clone, Debug, Default)]
pub struct Output {
    /// The composited frame at the output size (`OutputFormat::Bgra`).
    pub bgra: Option<Arc<Frame>>,
    /// The composited frame at the output size as NV12 (`OutputFormat::Nv12`).
    pub nv12: Option<Arc<Nv12Frame>>,
    /// NV12 for the virtual camera, fitted into 1920x1080 (the same Arc as
    /// `nv12` when the output already fits).
    pub camera: Option<Arc<Nv12Frame>>,
    /// A BGRA frame for the admin preview, only on ticks where one is due
    /// (`OutputFormat::Nv12`; with `Bgra` the preview samples `bgra`).
    pub preview: Option<Arc<Frame>>,
}

/// A destination for output frames (virtual camera, stream encoder, ...).
pub trait FrameSink: Send {
    fn push(&mut self, out: &Output) -> anyhow::Result<()>;
}

/// What a producer composites on one tick.
#[derive(Clone, Debug)]
pub struct TickInputs {
    /// The newest page frame (`None` without a page, and on the GPU path,
    /// whose producer reads its own page texture).
    pub page: Option<Arc<Frame>>,
    /// The webcam frame, if fresh.
    pub cam: Option<Arc<Frame>>,
    /// The cut-out mask, if fresh.
    pub mask: Option<Arc<Mask>>,
    pub layout: PipLayout,
}

/// Turns the tick's inputs into an `Output` (the CPU compositor now, D3D11 later).
pub trait OutputProducer: Send {
    /// `Ok(None)`: nothing changed since the last output; the ticker pushes
    /// the previous `Output` again.
    fn produce(&mut self, inputs: &TickInputs, preview_due: bool)
        -> anyhow::Result<Option<Output>>;
    /// Short name for logs ("CPU", ...).
    fn name(&self) -> String;
}

/// Composited frames that sinks may hold at once (the ticker's previous
/// output, the stream slot, the preview slot) plus one to draw into.
const POOL: usize = 4;

/// The CPU compositor as an `OutputProducer`. With `OutputFormat::Nv12` it
/// also converts to NV12 (BT.709) for the stream and the camera.
pub struct CpuProducer {
    spec: OutputSpec,
    compositor: Compositor,
    /// Reusable output frames; one is drawn into once nobody else holds it.
    pool: Vec<Arc<Frame>>,
    next_evict: usize,
    /// The last output (without `preview`) and its BGRA frame.
    last: Option<(Output, Arc<Frame>)>,
    /// A frame changed on a tick without a preview; send it on the next due tick.
    preview_pending: bool,
}

impl CpuProducer {
    pub fn new(spec: OutputSpec) -> CpuProducer {
        CpuProducer {
            spec,
            compositor: Compositor::new(),
            pool: Vec::with_capacity(POOL),
            next_evict: 0,
            last: None,
            preview_pending: false,
        }
    }

    /// Index of a pool frame nobody else holds (allocating one if needed).
    fn free_frame(&mut self) -> usize {
        if let Some(i) = self.pool.iter_mut().position(|f| Arc::get_mut(f).is_some()) {
            return i;
        }
        let fresh = Arc::new(Frame::new_black(self.spec.width, self.spec.height));
        if self.pool.len() < POOL {
            self.pool.push(fresh);
            self.pool.len() - 1
        } else {
            // Every frame is still held elsewhere: let go of one of ours.
            let i = self.next_evict % POOL;
            self.next_evict += 1;
            self.pool[i] = fresh;
            i
        }
    }
}

impl OutputProducer for CpuProducer {
    fn produce(
        &mut self,
        inputs: &TickInputs,
        preview_due: bool,
    ) -> anyhow::Result<Option<Output>> {
        let pip = inputs.cam.as_ref().map(|cam| Pip {
            cam,
            mask: inputs.mask.as_ref(),
            layout: &inputs.layout,
        });
        let i = self.free_frame();
        let target = Arc::get_mut(&mut self.pool[i]).expect("free pool frame");
        if !self.compositor.compose(inputs.page.as_ref(), pip, target) {
            // Unchanged, but the preview may still lack the latest frame.
            if preview_due && self.preview_pending {
                if let Some((out, frame)) = &self.last {
                    self.preview_pending = false;
                    return Ok(Some(Output {
                        preview: Some(frame.clone()),
                        ..out.clone()
                    }));
                }
            }
            return Ok(None);
        }
        let frame = self.pool[i].clone();
        let (w, h) = (self.spec.width, self.spec.height);
        let mut out = match self.spec.format {
            OutputFormat::Bgra => Output {
                bgra: Some(frame.clone()),
                ..Output::default()
            },
            OutputFormat::Nv12 => {
                anyhow::ensure!(
                    w > 0 && h > 0 && w.is_multiple_of(2) && h.is_multiple_of(2),
                    "NV12 output needs an even size (got {w}x{h})"
                );
                let nv12 = Arc::new(bgra_to_nv12(&frame, w, h));
                let camera = match camera_size(w, h) {
                    // Extreme aspect ratio: no camera frame rather than a panic.
                    (0, _) | (_, 0) => None,
                    s if s == (w, h) => Some(nv12.clone()),
                    (cw, ch) => Some(Arc::new(bgra_to_nv12(&frame, cw, ch))),
                };
                Output {
                    nv12: Some(nv12),
                    camera,
                    ..Output::default()
                }
            }
        };
        let last = out.clone();
        if self.spec.format == OutputFormat::Nv12 {
            if preview_due {
                out.preview = Some(frame.clone());
            }
            self.preview_pending = !preview_due;
        }
        self.last = Some((last, frame));
        Ok(Some(out))
    }

    fn name(&self) -> String {
        "CPU".into()
    }
}

/// Runs compositing at a fixed rate on its own thread.
pub struct Ticker {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl Ticker {
    /// `producer` turns each tick's inputs into an `Output` for the sinks.
    /// `page`: the page frames (`None` when the producer has its own source).
    /// `webcam`: optional PIP source; a webcam frame not updated for 1 s is treated
    /// as absent. `mask`: the latest cut-out mask, used by cut-out styles (same
    /// 1 s staleness; without a fresh one the webcam is drawn as a box, never a
    /// frozen cut-out). `layout` may be changed live. A preview is due every
    /// `max(1, fps / 12)` ticks.
    pub fn spawn(
        spec: OutputSpec,
        mut producer: Box<dyn OutputProducer>,
        page: Option<FrameSlot>,
        webcam: Option<FrameSlot>,
        mask: Option<MaskSlot>,
        layout: Arc<RwLock<PipLayout>>,
        mut sinks: Vec<Box<dyn FrameSink>>,
    ) -> Ticker {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = std::thread::Builder::new()
            .name("p2c-ticker".into())
            .spawn(move || {
                let period = Duration::from_secs_f64(1.0 / spec.fps.max(1) as f64);
                let preview_every = (spec.fps / PREVIEW_FPS).max(1);
                let name = producer.name();
                let mut prev: Option<Output> = None;
                let mut last_warn: Vec<Option<Instant>> = vec![None; sinks.len()];
                let mut produce_warn: Option<Instant> = None;
                let start = Instant::now();
                let mut n: u32 = 0;
                let mut k: u32 = 0;
                let mut cam_fresh = Freshness::default();
                let mut mask_fresh = Freshness::default();
                let (mut compose_t, mut sinks_t) =
                    (DurationStats::default(), DurationStats::default());
                let (mut ticks, mut redrawn, mut skipped) = (0u32, 0u32, 0u32);
                let mut last_log = Instant::now();
                while !stop2.load(Ordering::Relaxed) {
                    let t0 = Instant::now();
                    let inputs = TickInputs {
                        page: page.as_ref().and_then(|p| p.latest()),
                        cam: webcam.as_ref().and_then(|s| cam_fresh.latest(s)),
                        mask: mask.as_ref().and_then(|s| mask_fresh.latest(s)),
                        layout: *layout.read(),
                    };
                    let preview_due = k.is_multiple_of(preview_every);
                    k = k.wrapping_add(1);
                    // Unchanged or failed: push the previous output again (same
                    // Arcs, no conversion), with its preview only when one is due.
                    let repeat = match producer.produce(&inputs, preview_due) {
                        Ok(Some(o)) => {
                            redrawn += 1;
                            prev = Some(o);
                            false
                        }
                        Ok(None) => true,
                        Err(e) => {
                            if produce_warn.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
                                tracing::warn!(producer = %name, error = %e, "output frame failed");
                                produce_warn = Some(Instant::now());
                            }
                            true
                        }
                    };
                    let t1 = Instant::now();
                    compose_t.record(t1 - t0);
                    let stripped;
                    let out = match &prev {
                        Some(p) if repeat && !preview_due && p.preview.is_some() => {
                            stripped = Output {
                                preview: None,
                                ..p.clone()
                            };
                            Some(&stripped)
                        }
                        p => p.as_ref(),
                    };
                    if let Some(out) = out {
                        for (i, sink) in sinks.iter_mut().enumerate() {
                            if let Err(e) = sink.push(out) {
                                let due = last_warn[i]
                                    .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
                                if due {
                                    tracing::warn!(sink = i, error = %e, "frame sink failed");
                                    last_warn[i] = Some(Instant::now());
                                }
                            }
                        }
                    }
                    // Absolute deadlines avoid drift; if we fell behind, skip missed ticks.
                    n += 1;
                    ticks += 1;
                    let now = Instant::now();
                    sinks_t.record(now - t1);
                    let mut deadline = start + period * n;
                    if deadline < now {
                        let next = (now.duration_since(start).as_secs_f64() / period.as_secs_f64())
                            as u32
                            + 1;
                        skipped += next - n;
                        n = next;
                        deadline = start + period * n;
                    }
                    if last_log.elapsed() >= LOG_EVERY {
                        if let (Some(c), Some(s)) =
                            (compose_t.take_summary(), sinks_t.take_summary())
                        {
                            tracing::info!(
                                ticks,
                                redrawn,
                                skipped,
                                "output ({name}): compose {c}; sinks {s}"
                            );
                        }
                        (ticks, redrawn, skipped) = (0, 0, 0);
                        last_log = Instant::now();
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
    use p2c_core::{Frame, FrameSlot, Mask, MaskSlot};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    struct Counter(Arc<AtomicUsize>);
    impl FrameSink for Counter {
        fn push(&mut self, _: &Output) -> anyhow::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct Failing;
    impl FrameSink for Failing {
        fn push(&mut self, _: &Output) -> anyhow::Result<()> {
            anyhow::bail!("broken sink")
        }
    }

    /// Keeps every pushed `Output` (cheap: Arcs).
    struct Recorder(Arc<parking_lot::Mutex<Vec<Output>>>);
    impl FrameSink for Recorder {
        fn push(&mut self, out: &Output) -> anyhow::Result<()> {
            self.0.lock().push(out.clone());
            Ok(())
        }
    }

    fn layout() -> Arc<parking_lot::RwLock<crate::PipLayout>> {
        Arc::new(parking_lot::RwLock::new(crate::PipLayout::default()))
    }

    /// The CPU path, as `main.rs` builds it.
    fn spawn(
        spec: OutputSpec,
        page: FrameSlot,
        webcam: Option<FrameSlot>,
        mask: Option<MaskSlot>,
        layout: Arc<parking_lot::RwLock<crate::PipLayout>>,
        sinks: Vec<Box<dyn FrameSink>>,
    ) -> Ticker {
        Ticker::spawn(
            spec,
            Box::new(CpuProducer::new(spec)),
            Some(page),
            webcam,
            mask,
            layout,
            sinks,
        )
    }

    type Samples = Arc<parking_lot::Mutex<Vec<(std::time::Instant, [u8; 4])>>>;

    /// Records the color at one pixel of `out.bgra` on every push.
    struct Sampler(Samples, u32, u32);
    impl FrameSink for Sampler {
        fn push(&mut self, out: &Output) -> anyhow::Result<()> {
            let f = out.bgra.as_ref().expect("bgra output");
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
        let spec = small_spec();
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
        let t = spawn(
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
            format: OutputFormat::Bgra,
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
        matte: Option<MaskSlot>,
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
        let t = spawn(
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
    fn ticker_cutout_uses_mask() {
        // An all-room mask: the person is cut away and the page shows through.
        let cam = FrameSlot::new();
        let matte = MaskSlot::new();
        let (c, m) = (cam.clone(), matte.clone());
        let seen = run_sampled(
            Some(cam),
            Some(matte),
            move || {
                c.put(Frame::from_bgra(16, 9, GREEN.repeat(16 * 9)).unwrap());
                m.put(Mask::new(4, 2, vec![0; 8]).unwrap());
            },
            400,
            200,
        );
        assert!(
            !seen.is_empty() && seen.iter().all(|p| *p == RED),
            "{seen:?}"
        );
    }

    #[test]
    fn ticker_cutout_falls_back_to_box_when_mask_stale() {
        // Raw webcam has alpha 0: only an opaque Box draw makes it visible.
        let cam = FrameSlot::new();
        let c = cam.clone();
        let seen = run_sampled(
            Some(cam),
            Some(MaskSlot::new()),
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
        let matte = MaskSlot::new();
        cam.put(Frame::from_bgra(16, 9, GREEN.repeat(16 * 9)).unwrap());
        matte.put(Mask::new(4, 2, vec![255; 8]).unwrap());
        let seen = run_sampled(Some(cam), Some(matte), || {}, 1600, 1400);
        assert!(
            !seen.is_empty() && seen.iter().all(|p| *p == RED),
            "{seen:?}"
        );
    }

    #[test]
    fn ticker_bgra_unchanged_on_bgra_format() {
        // Linux path: BGRA every tick, nothing else.
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let t = spawn(
            small_spec(),
            red_page(),
            None,
            None,
            layout(),
            vec![Box::new(Recorder(seen.clone()))],
        );
        std::thread::sleep(Duration::from_millis(200));
        t.stop();
        let seen = seen.lock();
        assert!(!seen.is_empty());
        for o in seen.iter() {
            let f = o.bgra.as_ref().expect("bgra");
            assert_eq!((f.width, f.height), (64, 36));
            assert_eq!(f.pixel(10, 10), RED);
            assert!(o.nv12.is_none() && o.camera.is_none() && o.preview.is_none());
        }
    }

    #[test]
    fn ticker_produces_nv12_when_asked() {
        let spec = OutputSpec {
            format: OutputFormat::Nv12,
            ..small_spec()
        };
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let t = spawn(
            spec,
            red_page(),
            None,
            None,
            layout(),
            vec![Box::new(Recorder(seen.clone()))],
        );
        std::thread::sleep(Duration::from_millis(200));
        t.stop();
        let seen = seen.lock();
        assert!(!seen.is_empty());
        for o in seen.iter() {
            assert!(o.bgra.is_none(), "bgra produced on the NV12 path");
            let n = o.nv12.as_ref().expect("nv12");
            assert_eq!((n.width, n.height), (64, 36));
            // BT.709 limited red: Y = 16 + 0.2126 * 219 = 62.6
            assert!(n.data[0].abs_diff(63) <= 1, "Y = {}", n.data[0]);
            // Fits 1080p: the camera gets the very same frame.
            let c = o.camera.as_ref().expect("camera");
            assert!(Arc::ptr_eq(n, c));
        }
        // The preview is the composited BGRA, on the preview cadence.
        let p = seen[0].preview.as_ref().expect("preview on the first tick");
        assert_eq!(p.pixel(10, 10), RED);
    }

    #[test]
    fn repeats_previous_output_when_unchanged() {
        for format in [OutputFormat::Bgra, OutputFormat::Nv12] {
            let spec = OutputSpec {
                format,
                ..small_spec()
            };
            let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
            let t = spawn(
                spec,
                red_page(),
                None,
                None,
                layout(),
                vec![Box::new(Recorder(seen.clone()))],
            );
            std::thread::sleep(Duration::from_millis(300));
            t.stop();
            let seen = seen.lock();
            assert!((7..=11).contains(&seen.len()), "{format:?}: {}", seen.len());
            let first = &seen[0];
            for o in seen.iter() {
                match format {
                    OutputFormat::Bgra => assert!(Arc::ptr_eq(
                        o.bgra.as_ref().unwrap(),
                        first.bgra.as_ref().unwrap()
                    )),
                    OutputFormat::Nv12 => assert!(Arc::ptr_eq(
                        o.nv12.as_ref().unwrap(),
                        first.nv12.as_ref().unwrap()
                    )),
                }
            }
        }
    }

    #[test]
    fn camera_frame_is_downscaled_above_1080p() {
        let spec = OutputSpec {
            width: 2560,
            height: 1440,
            fps: 30,
            format: OutputFormat::Nv12,
        };
        let mut p = CpuProducer::new(spec);
        let page = Arc::new(Frame::from_bgra(2560, 1440, RED.repeat(2560 * 1440)).unwrap());
        let inputs = TickInputs {
            page: Some(page),
            cam: None,
            mask: None,
            layout: crate::PipLayout::default(),
        };
        let out = p.produce(&inputs, false).unwrap().unwrap();
        let n = out.nv12.unwrap();
        let c = out.camera.unwrap();
        assert_eq!((n.width, n.height), (2560, 1440));
        assert_eq!((c.width, c.height), (1920, 1080));
        assert!(out.preview.is_none() && out.bgra.is_none());
    }

    const SPEC: OutputSpec = OutputSpec {
        width: 16,
        height: 16,
        fps: 30,
        format: OutputFormat::Bgra,
    };

    #[test]
    fn ticker_runs_at_fps() {
        let n = Arc::new(AtomicUsize::new(0));
        let t = spawn(
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
        let t = spawn(SPEC, FrameSlot::new(), None, None, layout(), sinks);
        std::thread::sleep(Duration::from_secs(1));
        t.stop();
        assert!(n.load(Ordering::SeqCst) >= 27);
    }
}
