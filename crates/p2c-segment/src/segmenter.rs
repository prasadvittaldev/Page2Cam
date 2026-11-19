//! The cut-out thread: latest webcam frame -> engine -> cleaned-up person
//! mask, published to the `out` slot. The compositor combines the newest mask
//! with the newest webcam frame, so the person moves at camera speed even
//! when masks come more slowly.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use p2c_core::{DurationStats, FrameSlot, MaskSlot, Nv12Gate, Nv12Slot};
use parking_lot::Mutex;

use crate::engine::{Engine, EngineChoice};
use crate::policy::{too_slow_note, FallbackPolicy, PolicyTimes};
use crate::post::{profile, to_mask, Smoother};
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
        out: MaskSlot,
        choice: EngineChoice,
    ) -> Segmenter {
        Self::spawn_nv12(
            opener,
            webcam,
            Nv12Slot::new(),
            Nv12Gate::new(),
            out,
            choice,
        )
    }

    /// [`Segmenter::spawn`] plus the camera's NV12 slot: when it holds the
    /// twin of the webcam frame being processed, the model input is built
    /// from it instead of from the BGRA frame.
    pub fn spawn_nv12(
        opener: Opener,
        webcam: FrameSlot,
        nv12: Nv12Slot,
        gate: Nv12Gate,
        out: MaskSlot,
        choice: EngineChoice,
    ) -> Segmenter {
        Self::spawn_full(
            opener,
            webcam,
            nv12,
            gate,
            out,
            choice,
            PolicyTimes::default(),
        )
    }

    /// [`Segmenter::spawn`] with other fallback/retry times (tests).
    #[cfg(test)]
    fn spawn_with(
        opener: Opener,
        webcam: FrameSlot,
        out: MaskSlot,
        choice: EngineChoice,
        times: PolicyTimes,
    ) -> Segmenter {
        Self::spawn_full(
            opener,
            webcam,
            Nv12Slot::new(),
            Nv12Gate::new(),
            out,
            choice,
            times,
        )
    }

    fn spawn_full(
        opener: Opener,
        webcam: FrameSlot,
        nv12: Nv12Slot,
        gate: Nv12Gate,
        out: MaskSlot,
        choice: EngineChoice,
        times: PolicyTimes,
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
            .spawn(move || Worker::new(opener, webcam, nv12, gate, out, choice, times).run(&sh))
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
/// A webcam gap longer than this starts the engine's memory afresh.
const STALL: Duration = Duration::from_secs(1);
/// After this many failed inferences in a row, reopen the engine.
const MAX_ERRORS: u32 = 10;
/// How often the cut-out rate is written to the log while running.
const LOG_EVERY: Duration = Duration::from_secs(30);

struct Worker {
    opener: Opener,
    webcam: FrameSlot,
    nv12: Nv12Slot,
    gate: Nv12Gate,
    /// Inferences fed from NV12 / all inferences, for the periodic log line.
    nv12_hits: u32,
    infers: u32,
    out: MaskSlot,
    engine: Option<Box<dyn Engine>>,
    smoother: Option<Smoother>,
    /// Engine time (resize + model), mask clean-up time, and time between
    /// published masks, for the periodic log line.
    infer_t: DurationStats,
    post_t: DurationStats,
    /// Time turning the frame into model input (NV12 or BGRA path).
    prep_t: DurationStats,
    cycle_t: DurationStats,
    last_mask_at: Option<Instant>,
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
    was_enabled: bool,
    pending_reset: bool,
    errors_in_row: u32,
    last_rate_log: Instant,
}

impl Worker {
    fn new(
        opener: Opener,
        webcam: FrameSlot,
        nv12: Nv12Slot,
        gate: Nv12Gate,
        out: MaskSlot,
        choice: EngineChoice,
        times: PolicyTimes,
    ) -> Worker {
        Worker {
            opener,
            webcam,
            nv12,
            gate,
            nv12_hits: 0,
            infers: 0,
            out,
            engine: None,
            smoother: None,
            infer_t: DurationStats::default(),
            post_t: DurationStats::default(),
            prep_t: DurationStats::default(),
            cycle_t: DurationStats::default(),
            last_mask_at: None,
            target: choice,
            active: None,
            device: None,
            generation: 0,
            open_failed_at: None,
            policy: FallbackPolicy::with_times(times),
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
            was_enabled: false,
            pending_reset: false,
            errors_in_row: 0,
            last_rate_log: Instant::now(),
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
            if enabled && !self.was_enabled && self.engine.is_some() {
                // Back from Box style: don't reuse memory of old frames.
                self.pending_reset = true;
            }
            self.was_enabled = enabled;
            self.gate.set(enabled);
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

    /// On the automatic Fast: retry Quality when its wait is over (R42),
    /// else keep the countdown in the note.
    fn auto_retry(&mut self) {
        let now = Instant::now();
        if self.policy.retry_due(now) {
            tracing::info!("retrying the Quality cut-out after an automatic switch to Fast");
            self.policy.retrying(now);
            self.target = EngineChoice::Quality;
            self.drop_engine();
            self.open_failed_at = None;
            self.note.clear();
        } else if let Some(left) = self.policy.retry_in(now) {
            if self.note.is_empty() || self.note.starts_with("Quality was too slow") {
                self.note = too_slow_note(left);
            }
        }
    }

    /// One unit of work. Returns how long to sleep, if anything.
    fn step(&mut self) -> Option<Duration> {
        self.auto_retry();
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
                    self.smoother = profile(self.target).smoothing.map(Smoother::new);
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
            let gap = now.duration_since(t);
            if gap > STALL {
                self.pending_reset = true;
            } else {
                let frames = (seq - self.last_seq).max(1) as u32;
                self.cam_interval = gap / frames;
            }
        }
        self.last_seq = seq;
        self.last_seq_at = Some(now);
        let (frame, frame_seq) = self.webcam.latest_with_seq();
        let frame = frame?;
        // The NV12 twin: tagged with the BGRA frame it came with, same size.
        let twin = self.nv12.latest().filter(|n| {
            n.bgra_seq == frame_seq
                && (n.frame.width, n.frame.height) == (frame.width, frame.height)
        });

        let engine = self.engine.as_mut()?;
        let size = (frame.width, frame.height);
        if self.last_size.is_some_and(|s| s != size) || self.pending_reset {
            engine.reset();
            if let Some(s) = &mut self.smoother {
                s.reset();
            }
            self.pending_reset = false;
        }
        self.last_size = Some(size);

        let t = Instant::now();
        let result = match &twin {
            Some(n) => engine.infer_nv12(&n.frame),
            None => engine.infer(&frame),
        };
        self.infers += 1;
        self.nv12_hits += twin.is_some() as u32;
        if let Some(p) = engine.take_prep() {
            self.prep_t.record(p);
        }
        let took = t.elapsed();
        self.infer_t.record(took);
        let edge = profile(self.active.unwrap_or(self.target)).edge;
        let smoother = &mut self.smoother;
        let result = result.and_then(|(values, mw, mh)| {
            let t = Instant::now();
            let values = match smoother {
                Some(s) => s.apply(&values, mw, mh),
                None => values,
            };
            let m = to_mask(&values, mw, mh, edge);
            self.post_t.record(t.elapsed());
            m
        });
        match result {
            Ok(mask) => {
                self.out.put(mask);
                let now = Instant::now();
                if let Some(prev) = self.last_mask_at.replace(now) {
                    self.cycle_t.record(now - prev);
                }
                self.latency = took;
                self.window_count += 1;
                self.errors_in_row = 0;
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
                self.errors_in_row += 1;
                if self.errors_in_row >= MAX_ERRORS {
                    // E.g. GPU lost after sleep: start the engine again (the
                    // runtime falls back to the CPU if the GPU won't open).
                    tracing::warn!(engine = ?self.active, "reopening the cut-out engine after repeated errors");
                    self.errors_in_row = 0;
                    self.drop_engine();
                    return None;
                }
            }
        }
        if self.active == Some(EngineChoice::Quality)
            && self.policy.observe(took, self.cam_interval, Instant::now())
        {
            let wait = self.policy.fell_back(Instant::now());
            tracing::warn!(
                infer_ms = took.as_millis() as u64,
                cam_ms = self.cam_interval.as_millis() as u64,
                retry_in_s = wait.as_secs(),
                "Quality was too slow (busy GPU?), switched to Fast; trying Quality again later"
            );
            // Not saved to the settings: the operator's choice stays Quality.
            self.target = EngineChoice::Fast;
            self.drop_engine();
            self.note = too_slow_note(wait);
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
        if self.last_rate_log.elapsed() >= LOG_EVERY {
            self.last_rate_log = Instant::now();
            let fmt = |s: Option<p2c_core::Summary>| s.map_or("-".to_string(), |s| s.to_string());
            let (nv12_hits, infers) = (self.nv12_hits, self.infers);
            self.nv12_hits = 0;
            self.infers = 0;
            let (infer, post, cycle, prep) = (
                fmt(self.infer_t.take_summary()),
                fmt(self.post_t.take_summary()),
                fmt(self.cycle_t.take_summary()),
                fmt(self.prep_t.take_summary()),
            );
            if let (Some(a), Some(d)) = (self.active, self.device) {
                tracing::info!(
                    engine = %a,
                    device = %d,
                    masks_per_sec = format!("{:.1}", self.masks_per_sec),
                    latency_ms = self.latency.as_millis() as u64,
                    cam_ms = self.cam_interval.as_millis() as u64,
                    "cut-out running: model {infer}; input prep {prep} (nv12 {nv12_hits}/{infers}); clean-up {post}; between masks {cycle}"
                );
            }
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
    use crate::policy::PolicyTimes;
    use p2c_core::{Frame, MaskSlot};
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
        fail: bool,
    }

    impl Engine for FakeEngine {
        fn infer(&mut self, _: &Frame) -> anyhow::Result<(Vec<f32>, u32, u32)> {
            std::thread::sleep(self.delay);
            anyhow::ensure!(!self.fail, "device lost");
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
                fail: false,
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
    fn segmenter_publishes_mask() {
        let (cam, out) = (FrameSlot::new(), MaskSlot::new());
        let _f = Feeder::start(cam.clone(), 64, 36);
        let s = Segmenter::spawn(
            opener(1, 1, Counts::default()),
            cam,
            out.clone(),
            EngineChoice::Quality,
        );
        s.set_enabled(true);
        ms(400);
        let m = out.latest().expect("no mask published");
        assert!(out.seq() > 0);
        assert_eq!((m.width, m.height), (2, 2));
        assert!(m.data.iter().all(|a| *a == 255));
        assert_eq!(m.edge, crate::post::profile(EngineChoice::Quality).edge);
        let st = s.status();
        assert_eq!(st.active, Some(EngineChoice::Quality));
        assert!(st.masks_per_sec >= 0.0);
        s.stop();
    }

    /// Policy times scaled down so the fallback/retry cycle fits in a test.
    fn short_times() -> PolicyTimes {
        PolicyTimes {
            window: Duration::from_millis(300),
            first_retry: Duration::from_millis(1000),
            max_retry: Duration::from_millis(4000),
        }
    }

    /// Poll the status until `f` holds (panics after `timeout`).
    fn wait_for(s: &Segmenter, timeout: Duration, f: impl Fn(&SegStatus) -> bool) -> SegStatus {
        let t = Instant::now();
        loop {
            let st = s.status();
            if f(&st) {
                return st;
            }
            assert!(t.elapsed() < timeout, "timed out, last status {st:?}");
            ms(5);
        }
    }

    #[test]
    fn segmenter_auto_falls_back_and_notes_it() {
        let (cam, out) = (FrameSlot::new(), MaskSlot::new());
        let _f = Feeder::start(cam.clone(), 64, 36);
        let s = Segmenter::spawn_with(
            opener(120, 1, Counts::default()),
            cam,
            out,
            EngineChoice::Quality,
            short_times(),
        );
        s.set_enabled(true);
        let st = wait_for(&s, Duration::from_secs(3), |st| {
            st.active == Some(EngineChoice::Fast)
        });
        assert_eq!(st.requested, EngineChoice::Quality);
        assert!(st.note.contains("too slow"), "{}", st.note);
        // A manual switch back to Quality is honoured (and re-armed).
        s.set_choice(EngineChoice::Quality);
        let st = wait_for(&s, Duration::from_secs(1), |st| {
            st.active == Some(EngineChoice::Quality)
        });
        assert_eq!(st.note, "", "a manual choice clears the note");
        s.stop();
    }

    #[test]
    fn segmenter_retries_quality_with_backoff() {
        let counts = Counts::default();
        let (cam, out) = (FrameSlot::new(), MaskSlot::new());
        let _f = Feeder::start(cam.clone(), 64, 36);
        let s = Segmenter::spawn_with(
            opener(120, 1, counts.clone()),
            cam,
            out,
            EngineChoice::Quality,
            short_times(),
        );
        s.set_enabled(true);
        let fast = |st: &SegStatus| st.active == Some(EngineChoice::Fast);
        let quality = |st: &SegStatus| st.active == Some(EngineChoice::Quality);
        let st = wait_for(&s, Duration::from_secs(3), fast);
        let fell_back = Instant::now();
        assert_eq!(
            st.note,
            "Quality was too slow (busy GPU) \u{2014} trying Quality again in 1 s"
        );
        // Quality again after the wait, not before.
        let st = wait_for(&s, Duration::from_secs(3), quality);
        let waited = fell_back.elapsed();
        assert!(
            waited >= Duration::from_millis(850),
            "retried after {waited:?}"
        );
        assert_eq!(st.requested, EngineChoice::Quality);
        assert_eq!(st.note, "", "no note while retrying Quality");
        // Still too slow: back to Fast, now waiting twice as long.
        let st = wait_for(&s, Duration::from_secs(3), fast);
        assert_eq!(
            st.note,
            "Quality was too slow (busy GPU) \u{2014} trying Quality again in 2 s"
        );
        // The note counts down.
        let st = wait_for(&s, Duration::from_secs(2), |st| st.note.ends_with(" 1 s"));
        assert!(fast(&st), "{st:?}");
        // A manual choice re-arms: Quality now, the next wait is 1 s again.
        s.set_choice(EngineChoice::Quality);
        wait_for(&s, Duration::from_secs(1), quality);
        let st = wait_for(&s, Duration::from_secs(3), fast);
        assert!(st.note.ends_with("again in 1 s"), "{}", st.note);
        s.stop();
    }
    #[test]
    fn segmenter_disabled_runs_no_inference() {
        let counts = Counts::default();
        let (cam, out) = (FrameSlot::new(), MaskSlot::new());
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
        let (cam, out) = (FrameSlot::new(), MaskSlot::new());
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
    fn segmenter_survives_crop_size_change() {
        use p2c_core::{apply_crop, Crop};
        let counts = Counts::default();
        let (cam, out) = (FrameSlot::new(), MaskSlot::new());
        let s = Segmenter::spawn(
            opener(1, 1, counts.clone()),
            cam.clone(),
            out.clone(),
            EngineChoice::Quality,
        );
        s.set_enabled(true);
        let crop = Crop {
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        };
        let full = || Frame::from_bgra(64, 36, [9, 9, 9, 0].repeat(64 * 36)).unwrap();
        for i in 0..12 {
            let f = full();
            cam.put(if i % 4 < 2 { f } else { apply_crop(f, &crop) });
            ms(40);
        }
        assert!(out.seq() > 0, "no mask published");
        assert!(
            counts.resets.load(Ordering::SeqCst) >= 1,
            "no reset on crop size change"
        );
        let seq = out.seq();
        cam.put(apply_crop(full(), &crop));
        ms(200);
        assert!(out.seq() > seq, "masks stopped after the size change");
        s.stop();
    }

    #[test]
    fn segmenter_resets_when_reenabled_or_after_stall() {
        let counts = Counts::default();
        let (cam, out) = (FrameSlot::new(), MaskSlot::new());
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
            assert_eq!(counts.resets.load(Ordering::SeqCst), 0);
            // Box style and back: old recurrent state must not be reused.
            s.set_enabled(false);
            ms(100);
            s.set_enabled(true);
            ms(200);
        }
        let after_reenable = counts.resets.load(Ordering::SeqCst);
        assert!(after_reenable >= 1, "no reset after re-enabling");
        // Webcam stalls for over a second, then comes back.
        ms(1300);
        let _f = Feeder::start(cam, 64, 36);
        ms(200);
        assert!(
            counts.resets.load(Ordering::SeqCst) > after_reenable,
            "no reset after a stall"
        );
        s.stop();
    }

    #[test]
    fn segmenter_reopens_engine_after_repeated_errors() {
        let counts = Counts::default();
        let c = counts.clone();
        let opener: Opener = Box::new(move |_| {
            c.opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeEngine {
                delay: Duration::from_millis(1),
                value: 1.0,
                counts: c.clone(),
                fail: true,
            }) as Box<dyn Engine>)
        });
        let (cam, out) = (FrameSlot::new(), MaskSlot::new());
        let _f = Feeder::start(cam.clone(), 64, 36);
        let s = Segmenter::spawn(opener, cam, out, EngineChoice::Quality);
        s.set_enabled(true);
        ms(1000);
        assert!(
            counts.opens.load(Ordering::SeqCst) >= 2,
            "engine never reopened"
        );
        assert!(
            s.status().note.contains("device lost"),
            "{}",
            s.status().note
        );
        s.stop();
    }

    #[test]
    fn segmenter_open_error_reported() {
        let (cam, out) = (FrameSlot::new(), MaskSlot::new());
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
        let (cam, out) = (FrameSlot::new(), MaskSlot::new());
        let _f = Feeder::start(cam.clone(), 640, 480);
        let s = Segmenter::spawn(
            opener(1, 1, Counts::default()),
            cam,
            out.clone(),
            EngineChoice::Fast,
        );
        s.set_enabled(true);
        ms(400);
        let m = out.latest().expect("no mask");
        assert_eq!(m.data, vec![255; 4]);
        assert_eq!(m.edge, crate::post::profile(EngineChoice::Fast).edge);
        s.stop();
    }
}

#[cfg(test)]
mod nv12_path_tests {
    use super::*;
    use p2c_core::{Frame, Nv12Capture, Nv12Frame, Nv12Gate};

    /// Records which entry point the segmenter used.
    struct Recorder(Arc<Mutex<Vec<&'static str>>>);
    impl Engine for Recorder {
        fn infer(&mut self, _: &Frame) -> anyhow::Result<(Vec<f32>, u32, u32)> {
            self.0.lock().push("bgra");
            Ok((vec![1.0; 4], 2, 2))
        }
        fn infer_nv12(&mut self, _: &Nv12Frame) -> anyhow::Result<(Vec<f32>, u32, u32)> {
            self.0.lock().push("nv12");
            Ok((vec![1.0; 4], 2, 2))
        }
        fn reset(&mut self) {}
        fn device(&self) -> Device {
            Device::Cpu
        }
    }

    /// One BGRA frame (seq 1) and an NV12 capture with `tag` and `size`; the
    /// path the engine saw.
    fn path_for(tag: u64, size: (u32, u32)) -> (Vec<&'static str>, Nv12Gate) {
        let (cam, nv, out) = (FrameSlot::new(), Nv12Slot::new(), MaskSlot::new());
        cam.put(Frame::new_black(16, 8));
        nv.put(Nv12Capture {
            bgra_seq: tag,
            frame: Nv12Frame::new(size.0, size.1, vec![0; (size.0 * size.1 * 3 / 2) as usize])
                .unwrap(),
        });
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let opener: Opener =
            Box::new(move |_| Ok(Box::new(Recorder(s2.clone())) as Box<dyn Engine>));
        let gate = Nv12Gate::new();
        let s = Segmenter::spawn_nv12(
            opener,
            cam,
            nv,
            gate.clone(),
            out.clone(),
            EngineChoice::Fast,
        );
        s.set_enabled(true);
        let t = Instant::now();
        while out.seq() == 0 && t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let wanted = gate.is_on();
        s.stop();
        let mut v = seen.lock().clone();
        v.push(if wanted { "gate-on" } else { "gate-off" });
        (v, gate)
    }

    #[test]
    fn segmenter_uses_nv12_only_for_a_matching_twin() {
        assert_eq!(path_for(1, (16, 8)).0, ["nv12", "gate-on"], "matching twin");
        assert_eq!(path_for(0, (16, 8)).0, ["bgra", "gate-on"], "stale tag");
        assert_eq!(path_for(1, (8, 4)).0, ["bgra", "gate-on"], "different size");
    }
}
