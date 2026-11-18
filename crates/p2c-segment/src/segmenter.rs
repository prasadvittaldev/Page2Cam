//! The cut-out thread: latest webcam frame -> engine -> same frame with the
//! mask in its alpha byte, published to the `out` slot.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use p2c_core::FrameSlot;
use parking_lot::Mutex;

use crate::engine::{Engine, EngineChoice};
use crate::policy::FallbackPolicy;
use crate::tensor::write_alpha;
use crate::Device;

/// Opens an engine; the real one is [`crate::open_engine`].
pub type Opener = Box<dyn Fn(EngineChoice) -> anyhow::Result<Box<dyn Engine>> + Send>;

/// What the admin page shows about the cut-out.
#[derive(Clone, Debug, PartialEq)]
pub struct SegStatus {
    /// What the operator chose.
    pub requested: EngineChoice,
    /// What is running (differs after an automatic fallback); None if no engine could start.
    pub active: Option<EngineChoice>,
    pub device: Option<Device>,
    pub masks_per_sec: f32,
    /// Inference time of the last mask, in ms (used to delay the mic to match).
    pub latency_ms: u32,
    /// Empty, or why things differ from what was asked.
    pub note: String,
}

pub struct Segmenter {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

struct Shared {
    ctl: Mutex<Ctl>,
    status: Mutex<SegStatus>,
    stop: AtomicBool,
}

struct Ctl {
    enabled: bool,
    choice: EngineChoice,
    /// Bumped on every manual switch.
    generation: u64,
}

impl Segmenter {
    /// Starts disabled; call [`Segmenter::set_enabled`] when a cut-out style is chosen.
    pub fn spawn(
        opener: Opener,
        webcam: FrameSlot,
        out: FrameSlot,
        choice: EngineChoice,
    ) -> Segmenter {
        let shared = Arc::new(Shared {
            ctl: Mutex::new(Ctl {
                enabled: false,
                choice,
                generation: 0,
            }),
            status: Mutex::new(SegStatus {
                requested: choice,
                active: None,
                device: None,
                masks_per_sec: 0.0,
                latency_ms: 0,
                note: String::new(),
            }),
            stop: AtomicBool::new(false),
        });
        let sh = shared.clone();
        let handle = std::thread::Builder::new()
            .name("p2c-segment".into())
            .spawn(move || Worker::new(opener, webcam, out, choice).run(&sh))
            .expect("spawn segmenter thread");
        Segmenter {
            shared,
            handle: Some(handle),
        }
    }

    pub fn set_enabled(&self, on: bool) {
        self.shared.ctl.lock().enabled = on;
    }

    /// Operator's choice. Re-arms the automatic fallback.
    pub fn set_choice(&self, choice: EngineChoice) {
        let mut c = self.shared.ctl.lock();
        c.choice = choice;
        c.generation += 1;
    }

    pub fn status(&self) -> SegStatus {
        self.shared.status.lock().clone()
    }

    /// Ask the thread to finish without waiting (for owners behind an `Arc`).
    pub fn request_stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }

    pub fn stop(mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Retry opening a failed engine this often.
const RETRY_OPEN: Duration = Duration::from_secs(5);
const IDLE: Duration = Duration::from_millis(50);
const POLL: Duration = Duration::from_millis(3);
const TOO_SLOW: &str = "Quality was too slow on this PC, switched to Fast";

struct Worker {
    opener: Opener,
    webcam: FrameSlot,
    out: FrameSlot,
    engine: Option<Box<dyn Engine>>,
    /// Engine we want running: the operator's choice, or Fast after a fallback.
    target: EngineChoice,
    active: Option<EngineChoice>,
    device: Option<Device>,
    generation: u64,
    open_failed_at: Option<Instant>,
    policy: FallbackPolicy,
    last_seq: u64,
    last_seq_at: Option<Instant>,
    cam_interval: Duration,
    last_size: Option<(u32, u32)>,
    note: String,
    latency: Duration,
    window_start: Instant,
    window_count: u32,
    masks_per_sec: f32,
    last_error_log: Option<Instant>,
}

impl Worker {
    fn new(opener: Opener, webcam: FrameSlot, out: FrameSlot, choice: EngineChoice) -> Worker {
        Worker {
            opener,
            webcam,
            out,
            engine: None,
            target: choice,
            active: None,
            device: None,
            generation: 0,
            open_failed_at: None,
            policy: FallbackPolicy::new(),
            last_seq: 0,
            last_seq_at: None,
            cam_interval: Duration::from_millis(33),
            last_size: None,
            note: String::new(),
            latency: Duration::ZERO,
            window_start: Instant::now(),
            window_count: 0,
            masks_per_sec: 0.0,
            last_error_log: None,
        }
    }

    fn run(mut self, sh: &Shared) {
        while !sh.stop.load(Ordering::Relaxed) {
            let (enabled, requested, generation) = {
                let c = sh.ctl.lock();
                (c.enabled, c.choice, c.generation)
            };
            if generation != self.generation {
                // Manual switch: fresh engine (and fresh recurrent state), re-armed fallback.
                self.generation = generation;
                self.target = requested;
                self.drop_engine();
                self.policy.rearm();
                self.open_failed_at = None;
                self.note.clear();
            }
            let pause = if enabled { self.step() } else { Some(IDLE) };
            if !enabled {
                self.masks_per_sec = 0.0;
                self.window_count = 0;
                self.window_start = Instant::now();
            }
            self.publish(sh, requested);
            if let Some(p) = pause {
                std::thread::sleep(p);
            }
        }
    }

    fn drop_engine(&mut self) {
        self.engine = None;
        self.active = None;
        self.device = None;
        self.last_size = None;
    }

    /// One unit of work. Returns how long to sleep, if anything.
    fn step(&mut self) -> Option<Duration> {
        if self.engine.is_none() {
            if self
                .open_failed_at
                .is_some_and(|t| t.elapsed() < RETRY_OPEN)
            {
                return Some(IDLE);
            }
            match (self.opener)(self.target) {
                Ok(e) => {
                    tracing::info!(engine = %self.target, device = %e.device(), "cut-out engine ready");
                    self.device = Some(e.device());
                    self.engine = Some(e);
                    self.active = Some(self.target);
                    self.open_failed_at = None;
                    if self.note.starts_with("cut-out unavailable") {
                        self.note.clear();
                    }
                }
                Err(e) => {
                    tracing::warn!(engine = %self.target, "cut-out unavailable: {e:#}");
                    self.note = format!("cut-out unavailable: {e:#}");
                    self.open_failed_at = Some(Instant::now());
                    return Some(IDLE);
                }
            }
        }
        self.roll_rate();

        let seq = self.webcam.seq();
        if seq == self.last_seq {
            return Some(POLL);
        }
        let now = Instant::now();
        if let (Some(t), true) = (self.last_seq_at, self.last_seq != 0) {
            let frames = (seq - self.last_seq).max(1) as u32;
            self.cam_interval = now.duration_since(t) / frames;
        }
        self.last_seq = seq;
        self.last_seq_at = Some(now);
        let frame = self.webcam.latest()?;

        let engine = self.engine.as_mut()?;
        let size = (frame.width, frame.height);
        if self.last_size.is_some_and(|s| s != size) {
            engine.reset();
        }
        self.last_size = Some(size);

        let t = Instant::now();
        let result = engine.infer(&frame);
        let took = t.elapsed();
        match result {
            Ok((mask, mw, mh)) => {
                let mut f = (*frame).clone();
                write_alpha(&mut f, &mask, mw, mh);
                self.out.put(f);
                self.latency = took;
                self.window_count += 1;
                if self.note.starts_with("cut-out error") {
                    self.note.clear();
                }
            }
            Err(e) => {
                self.note = format!("cut-out error: {e:#}");
                if self
                    .last_error_log
                    .is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
                {
                    tracing::warn!("cut-out error: {e:#}");
                    self.last_error_log = Some(Instant::now());
                }
            }
        }
        if self.active == Some(EngineChoice::Quality)
            && self.policy.observe(took, self.cam_interval, Instant::now())
        {
            tracing::warn!(
                infer_ms = took.as_millis() as u64,
                cam_ms = self.cam_interval.as_millis() as u64,
                "{TOO_SLOW}"
            );
            self.target = EngineChoice::Fast;
            self.drop_engine();
            self.note = TOO_SLOW.to_string();
        }
        None
    }

    fn roll_rate(&mut self) {
        let el = self.window_start.elapsed();
        if el >= Duration::from_secs(1) {
            self.masks_per_sec = self.window_count as f32 / el.as_secs_f32();
            self.window_count = 0;
            self.window_start = Instant::now();
        }
    }

    fn publish(&self, sh: &Shared, requested: EngineChoice) {
        *sh.status.lock() = SegStatus {
            requested,
            active: self.active,
            device: self.device,
            masks_per_sec: self.masks_per_sec,
            latency_ms: self.latency.as_millis() as u32,
            note: self.note.clone(),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2c_core::Frame;
    use std::sync::atomic::AtomicUsize;

    #[derive(Clone, Default)]
    struct Counts {
        infers: Arc<AtomicUsize>,
        resets: Arc<AtomicUsize>,
        opens: Arc<AtomicUsize>,
    }

    struct FakeEngine {
        delay: Duration,
        value: f32,
        counts: Counts,
    }

    impl Engine for FakeEngine {
        fn infer(&mut self, _: &Frame) -> anyhow::Result<(Vec<f32>, u32, u32)> {
            std::thread::sleep(self.delay);
            self.counts.infers.fetch_add(1, Ordering::SeqCst);
            Ok((vec![self.value; 4], 2, 2))
        }
        fn reset(&mut self) {
            self.counts.resets.fetch_add(1, Ordering::SeqCst);
        }
        fn device(&self) -> Device {
            Device::Cpu
        }
    }

    fn opener(quality_ms: u64, fast_ms: u64, counts: Counts) -> Opener {
        Box::new(move |c| {
            counts.opens.fetch_add(1, Ordering::SeqCst);
            let delay = Duration::from_millis(match c {
                EngineChoice::Quality => quality_ms,
                EngineChoice::Fast => fast_ms,
            });
            Ok(Box::new(FakeEngine {
                delay,
                value: 1.0,
                counts: counts.clone(),
            }) as Box<dyn Engine>)
        })
    }

    /// Puts a `w` x `h` frame (alpha 0) into `slot` at ~30 fps until dropped.
    struct Feeder(Arc<AtomicBool>, Option<JoinHandle<()>>);
    impl Feeder {
        fn start(slot: FrameSlot, w: u32, h: u32) -> Feeder {
            let stop = Arc::new(AtomicBool::new(false));
            let s = stop.clone();
            let h = std::thread::spawn(move || {
                while !s.load(Ordering::SeqCst) {
                    slot.put(
                        Frame::from_bgra(w, h, [9, 9, 9, 0].repeat((w * h) as usize)).unwrap(),
                    );
                    std::thread::sleep(Duration::from_millis(33));
                }
            });
            Feeder(stop, Some(h))
        }
    }
    impl Drop for Feeder {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
            if let Some(h) = self.1.take() {
                let _ = h.join();
            }
        }
    }

    fn ms(n: u64) {
        std::thread::sleep(Duration::from_millis(n));
    }

    #[test]
    fn segmenter_writes_alpha_into_out_slot() {
        let (cam, out) = (FrameSlot::new(), FrameSlot::new());
        let _f = Feeder::start(cam.clone(), 64, 36);
        let s = Segmenter::spawn(
            opener(1, 1, Counts::default()),
            cam,
            out.clone(),
            EngineChoice::Quality,
        );
        s.set_enabled(true);
        ms(400);
        let f = out.latest().expect("no matte published");
        assert!(out.seq() > 0);
        assert!(f
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[3] == 255 && p[0] == 9));
        let st = s.status();
        assert_eq!(st.active, Some(EngineChoice::Quality));
        assert!(st.masks_per_sec >= 0.0);
        s.stop();
    }

    #[test]
    fn segmenter_auto_falls_back_and_notes_it() {
        let (cam, out) = (FrameSlot::new(), FrameSlot::new());
        let _f = Feeder::start(cam.clone(), 64, 36);
        let s = Segmenter::spawn(
            opener(120, 1, Counts::default()),
            cam,
            out,
            EngineChoice::Quality,
        );
        s.set_enabled(true);
        ms(4500);
        let st = s.status();
        assert_eq!(st.requested, EngineChoice::Quality);
        assert_eq!(st.active, Some(EngineChoice::Fast), "{st:?}");
        assert!(st.note.contains("too slow"), "{}", st.note);
        // A manual switch back to Quality is honoured (and re-armed).
        s.set_choice(EngineChoice::Quality);
        ms(300);
        assert_eq!(s.status().active, Some(EngineChoice::Quality));
        s.stop();
    }

    #[test]
    fn segmenter_disabled_runs_no_inference() {
        let counts = Counts::default();
        let (cam, out) = (FrameSlot::new(), FrameSlot::new());
        let _f = Feeder::start(cam.clone(), 64, 36);
        let s = Segmenter::spawn(opener(1, 1, counts.clone()), cam, out, EngineChoice::Fast);
        ms(300);
        assert_eq!(counts.infers.load(Ordering::SeqCst), 0);
        s.set_enabled(true);
        ms(300);
        assert!(counts.infers.load(Ordering::SeqCst) > 0);
        s.set_enabled(false);
        ms(150);
        let n = counts.infers.load(Ordering::SeqCst);
        ms(300);
        assert_eq!(counts.infers.load(Ordering::SeqCst), n);
        assert_eq!(s.status().masks_per_sec, 0.0);
        s.stop();
    }

    #[test]
    fn segmenter_resets_on_switch_and_size_change() {
        let counts = Counts::default();
        let (cam, out) = (FrameSlot::new(), FrameSlot::new());
        let s = Segmenter::spawn(
            opener(1, 1, counts.clone()),
            cam.clone(),
            out,
            EngineChoice::Quality,
        );
        s.set_enabled(true);
        {
            let _f = Feeder::start(cam.clone(), 64, 36);
            ms(200);
        }
        assert_eq!(counts.resets.load(Ordering::SeqCst), 0);
        {
            let _f = Feeder::start(cam.clone(), 32, 18);
            ms(200);
        }
        assert!(
            counts.resets.load(Ordering::SeqCst) >= 1,
            "no reset on size change"
        );
        let opens = counts.opens.load(Ordering::SeqCst);
        s.set_choice(EngineChoice::Fast);
        let _f = Feeder::start(cam, 32, 18);
        ms(200);
        assert!(
            counts.opens.load(Ordering::SeqCst) > opens,
            "switch did not open a fresh engine"
        );
        s.stop();
    }

    #[test]
    fn segmenter_open_error_reported() {
        let (cam, out) = (FrameSlot::new(), FrameSlot::new());
        let _f = Feeder::start(cam.clone(), 64, 36);
        let s = Segmenter::spawn(
            Box::new(|_| anyhow::bail!("model missing: x.onnx")),
            cam,
            out.clone(),
            EngineChoice::Quality,
        );
        s.set_enabled(true);
        ms(300);
        let st = s.status();
        assert!(st.note.contains("x.onnx"), "{}", st.note);
        assert_eq!(st.active, None);
        assert_eq!(out.seq(), 0);
        s.stop();
    }

    #[test]
    fn segmenter_handles_640x480() {
        let (cam, out) = (FrameSlot::new(), FrameSlot::new());
        let _f = Feeder::start(cam.clone(), 640, 480);
        let s = Segmenter::spawn(
            opener(1, 1, Counts::default()),
            cam,
            out.clone(),
            EngineChoice::Fast,
        );
        s.set_enabled(true);
        ms(400);
        let f = out.latest().expect("no matte");
        assert_eq!((f.width, f.height), (640, 480));
        assert_eq!(f.data[640 * 480 * 4 - 1], 255);
        s.stop();
    }
}
