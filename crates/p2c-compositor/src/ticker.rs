use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use p2c_core::{
    bgra_to_nv12, camera_size, DurationStats, Frame, FrameSlot, Mask, MaskSlot, Nv12Frame, Slot,
};

use parking_lot::RwLock;

use crate::{Backdrop, ComposeNote, Compositor, Pip, PipLayout};

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
    /// Backgrounds and edge quality (`Backdrop::default()`: page, room, LUT edges).
    pub backdrop: Backdrop,
}

/// Turns the tick's inputs into an `Output` (the CPU compositor now, D3D11 later).
pub trait OutputProducer: Send {
    /// `Ok(None)`: nothing changed since the last output; the ticker pushes
    /// the previous `Output` again.
    fn produce(&mut self, inputs: &TickInputs, preview_due: bool)
        -> anyhow::Result<Option<Output>>;
    /// Short name for logs ("CPU", ...).
    fn name(&self) -> String;
    /// Whether the ticker may replace this producer with the CPU compositor
    /// after repeated failures (false for the CPU compositor itself).
    fn can_fall_back(&self) -> bool {
        true
    }
    /// Ticks between composing a frame and the tick whose `Output` carries
    /// it (a GPU readback ring maps the frame from N-2 ticks ago). 0 when the
    /// output is available on the tick that composed it.
    fn pipeline_frames(&self) -> u32 {
        0
    }
    /// Since when this producer has been holding its output (`Ok(None)`
    /// while it recovers, e.g. rebuilding a lost GPU device), if it is. A
    /// fallback that follows counts its wait for the CPU page from then, so
    /// the two waits don't add up.
    fn holding_since(&self) -> Option<Instant> {
        None
    }
    /// Whether this tick's composite contains a real page frame (not the
    /// black stand-in before the first paint). Default: the CPU page slot
    /// has a frame; a producer with its own page source overrides it.
    fn has_page(&self, inputs: &TickInputs) -> bool {
        inputs.page.is_some()
    }
}

/// How many ticks of the last `PIPELINE_WINDOW` ticks are looked at for the
/// pipeline latency.
const PIPELINE_WINDOW: usize = 60;

/// Median of the recent `pipeline_frames` values times the tick period (ms,
/// whole ms, rounded down); 0 without samples.
pub(crate) fn median_pipeline_ms(frames: &[u32], fps: u32) -> u32 {
    if frames.is_empty() {
        return 0;
    }
    let mut v = frames.to_vec();
    v.sort_unstable();
    (v[v.len() / 2] as u64 * 1000 / fps.max(1) as u64) as u32
}

/// What the ticker reports about its producer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProducerEvent {
    /// The producer failed (twice in a row, or on request) and the ticker
    /// switched to the CPU compositor. The text says why.
    Failed(String),
}

/// Asks a running ticker to switch to the CPU compositor (e.g. when the
/// browser never delivers GPU frames). Cloneable; only the first request counts.
#[derive(Clone, Default)]
pub struct FallbackTrigger(Arc<parking_lot::Mutex<TriggerState>>);

#[derive(Default)]
struct TriggerState {
    reason: Option<String>,
    /// A request was made (later ones are ignored).
    requested: bool,
}

impl FallbackTrigger {
    pub fn fall_back(&self, reason: impl Into<String>) {
        let mut s = self.0.lock();
        if !s.requested {
            s.requested = true;
            s.reason = Some(reason.into());
        }
    }

    /// The pending request, once.
    fn take(&self) -> Option<String> {
        self.0.lock().reason.take()
    }

    /// Mark as used without a request (the ticker fell back on its own).
    fn close(&self) {
        let mut s = self.0.lock();
        s.requested = true;
        s.reason = None;
    }
}

/// Whether `o` carries another frame than `prev` (a re-pushed or replayed
/// output, e.g. the same frame with a late preview, is not a redraw).
fn is_redraw(prev: Option<&Output>, o: &Output) -> bool {
    fn same<T>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
    !prev.is_some_and(|p| same(&p.bgra, &o.bgra) && same(&p.nv12, &o.nv12))
}

/// After a fallback, how long to keep the last frame while the CPU page
/// (the recreated browser's first paint) is missing. After that the CPU
/// compositor runs without a page (black, live webcam) until the page comes:
/// the camera never freezes for longer than this. Counted from when the
/// failed producer began holding its output
/// ([`OutputProducer::holding_since`]), so a timed-out device rebuild and
/// this wait together stay at about 1 s.
const WAIT_FOR_CPU_PAGE: Duration = Duration::from_secs(1);

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
        let (w, h) = (self.spec.width, self.spec.height);
        // Check before taking a target: that may allocate a whole frame.
        if self
            .compositor
            .is_unchanged(inputs.page.as_ref(), pip, &inputs.backdrop, w, h)
        {
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
        let i = self.free_frame();
        let target = Arc::get_mut(&mut self.pool[i]).expect("free pool frame");
        self.compositor
            .compose(inputs.page.as_ref(), pip, &inputs.backdrop, target);
        let frame = self.pool[i].clone();
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

    fn can_fall_back(&self) -> bool {
        false
    }
}

/// A page that never paints still hands the camera over after this long.
pub(crate) const READY_FALLBACK: Duration = Duration::from_secs(10);

/// Why the output became ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadyWhy {
    /// An output carrying a real page frame went to the sinks.
    Page,
    /// [`READY_FALLBACK`] passed without one.
    Timeout,
}

/// When `output_ready` is reported (the tray hands the camera to the engine
/// then): once an output that carries a real page frame (a Chromium paint,
/// or a test-pattern frame) went to the sinks, not the black frames before
/// the first paint; or after [`READY_FALLBACK`] anyway.
#[derive(Debug)]
pub(crate) struct ReadyGate {
    since: Instant,
    /// Tick on which the producer first had a page.
    page_tick: Option<u64>,
    done: bool,
}

impl ReadyGate {
    pub(crate) fn new(since: Instant) -> ReadyGate {
        ReadyGate {
            since,
            page_tick: None,
            done: false,
        }
    }

    /// An output was pushed on `tick`; `has_page`: the producer had a page
    /// this tick; `pipeline`: ticks between composing and pushing a frame.
    /// `Some` the first time the output counts as ready.
    pub(crate) fn pushed(
        &mut self,
        tick: u64,
        has_page: bool,
        pipeline: u32,
        now: Instant,
    ) -> Option<ReadyWhy> {
        if self.done {
            return None;
        }
        if has_page && self.page_tick.is_none() {
            self.page_tick = Some(tick);
        }
        let why = if self
            .page_tick
            .is_some_and(|t| tick >= t + u64::from(pipeline))
        {
            ReadyWhy::Page
        } else if now.saturating_duration_since(self.since) >= READY_FALLBACK {
            ReadyWhy::Timeout
        } else {
            return None;
        };
        self.done = true;
        Some(why)
    }
}

/// Running totals for health views (readers take differences over time).
#[derive(Default, Debug)]
pub struct TickerStats {
    ticks: AtomicU64,
    skipped: AtomicU64,
    output_ready: AtomicBool,
    /// [`ComposeNote`] of the latest tick, as `note_code`.
    note: AtomicU8,
}

fn note_code(n: ComposeNote) -> u8 {
    match n {
        ComposeNote::None => 0,
        ComposeNote::CutoutNotReady => 1,
        ComposeNote::WebcamOff => 2,
    }
}

impl TickerStats {
    /// Why the output doesn't show the chosen backdrop right now (for
    /// `Status.background_note`); the same on the CPU and GPU paths.
    pub fn compose_note(&self) -> ComposeNote {
        match self.note.load(Ordering::Relaxed) {
            1 => ComposeNote::CutoutNotReady,
            2 => ComposeNote::WebcamOff,
            _ => ComposeNote::None,
        }
    }

    /// Ticks run since start.
    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Relaxed)
    }
    /// Ticks missed (deadline already past) since start.
    pub fn skipped(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }
    /// True once the first output frame went to the sinks.
    pub fn output_ready(&self) -> bool {
        self.output_ready.load(Ordering::Acquire)
    }
}

/// Runs compositing at a fixed rate on its own thread.
pub struct Ticker {
    stop: Arc<AtomicBool>,
    stats: Arc<TickerStats>,
    pipeline_ms: Arc<AtomicU32>,
    handle: JoinHandle<()>,
    trigger: FallbackTrigger,
}

/// The ticker's producer, replaced by the CPU compositor after it fails twice
/// in a row (or on request).
struct ProducerSlot {
    spec: OutputSpec,
    producer: Box<dyn OutputProducer>,
    name: String,
    /// Consecutive `produce` errors.
    errors: u32,
    fell_back: bool,
    /// After a fallback: keep the last frame until the CPU page arrives.
    wait_for_page: Option<Instant>,
    events: Sender<ProducerEvent>,
    trigger: FallbackTrigger,
    warned: Option<Instant>,
}

impl ProducerSlot {
    fn produce(
        &mut self,
        inputs: &TickInputs,
        preview_due: bool,
        has_prev: bool,
    ) -> anyhow::Result<Option<Output>> {
        if let Some(reason) = self.trigger.take() {
            self.fall_back(reason, has_prev);
        }
        let r = self.produce_once(inputs, preview_due);
        let Err(e) = r else {
            self.errors = 0;
            return r;
        };
        self.errors += 1;
        if self
            .warned
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            tracing::warn!(producer = %self.name, error = %format!("{e:#}"), "output frame failed");
            self.warned = Some(Instant::now());
        }
        // One failure is retried on the next tick (a GPU producer recreates
        // its device in between); a second one in a row switches to the CPU.
        if self.errors >= 2 && !self.fell_back && self.producer.can_fall_back() {
            self.fall_back(format!("{e:#}"), has_prev);
            return self.produce_once(inputs, preview_due);
        }
        Err(e)
    }

    fn produce_once(
        &mut self,
        inputs: &TickInputs,
        preview_due: bool,
    ) -> anyhow::Result<Option<Output>> {
        if let Some(since) = self.wait_for_page {
            if inputs.page.is_none() && since.elapsed() < WAIT_FOR_CPU_PAGE {
                return Ok(None);
            }
            self.wait_for_page = None;
        }
        self.producer.produce(inputs, preview_due)
    }

    fn fall_back(&mut self, reason: String, has_prev: bool) {
        self.trigger.close();
        if self.fell_back || !self.producer.can_fall_back() {
            tracing::info!(producer = %self.name, %reason, "fallback request ignored");
            return;
        }
        tracing::warn!(from = %self.name, %reason, "video falls back to the CPU compositor");
        // The output has been frozen since the old producer began holding
        // it (e.g. a timed-out device rebuild): the page wait counts from then.
        let frozen_since = self
            .producer
            .holding_since()
            .unwrap_or_else(Instant::now)
            .min(Instant::now());
        self.producer = Box::new(CpuProducer::new(self.spec));
        self.name = self.producer.name();
        self.fell_back = true;
        self.errors = 0;
        self.wait_for_page = has_prev.then_some(frozen_since);
        let _ = self.events.send(ProducerEvent::Failed(reason));
    }
}

impl Ticker {
    /// `page`: the page frames for the CPU compositor (also used after a
    /// fallback; a GPU producer reads its own page texture).
    /// `webcam`: optional PIP source; a webcam frame not updated for 1 s is treated
    /// as absent. `mask`: the latest cut-out mask, used by cut-out styles (same
    /// 1 s staleness; without a fresh one the webcam is drawn as a box, never a
    /// frozen cut-out). `layout` and `backdrop` may be changed live; the
    /// backdrop's fallback note is in [`TickerStats::compose_note`]. `producer` turns each
    /// tick's inputs into an `Output` for the sinks. A preview is due every
    /// `max(1, fps / 12)` ticks.
    ///
    /// A failed tick pushes the previous output again. If `produce` fails twice
    /// in a row, or [`FallbackTrigger::fall_back`] is called, the ticker
    /// switches to a [`CpuProducer`] for good and sends
    /// [`ProducerEvent::Failed`] on `events` (once). Until the first CPU page
    /// frame arrives it keeps pushing the last output for up to 1 s, then
    /// composites without a page (black, live webcam).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        spec: OutputSpec,
        page: Option<FrameSlot>,
        webcam: Option<FrameSlot>,
        mask: Option<MaskSlot>,
        layout: Arc<RwLock<PipLayout>>,
        backdrop: Arc<RwLock<Backdrop>>,
        producer: Box<dyn OutputProducer>,
        mut sinks: Vec<Box<dyn FrameSink>>,
        events: Sender<ProducerEvent>,
    ) -> Ticker {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let trigger = FallbackTrigger::default();
        let pipeline_ms = Arc::new(AtomicU32::new(0));
        let pipeline_out = pipeline_ms.clone();
        let stats = Arc::new(TickerStats::default());
        let stats_out = stats.clone();
        let mut slot = ProducerSlot {
            spec,
            name: producer.name(),
            producer,
            errors: 0,
            fell_back: false,
            wait_for_page: None,
            events,
            trigger: trigger.clone(),
            warned: None,
        };
        let handle = std::thread::Builder::new()
            .name("p2c-ticker".into())
            .spawn(move || {
                let period = Duration::from_secs_f64(1.0 / spec.fps.max(1) as f64);
                let preview_every = (spec.fps / PREVIEW_FPS).max(1);
                let mut prev: Option<Output> = None;
                let mut last_warn: Vec<Option<Instant>> = vec![None; sinks.len()];
                let start = Instant::now();
                let mut ready = ReadyGate::new(start);
                let mut tick_no: u64 = 0;
                let mut n: u32 = 0;
                let mut k: u32 = 0;
                let mut cam_fresh = Freshness::default();
                let mut mask_fresh = Freshness::default();
                let mut pipe_frames: std::collections::VecDeque<u32> =
                    std::collections::VecDeque::with_capacity(PIPELINE_WINDOW + 1);
                let (mut compose_t, mut sinks_t) =
                    (DurationStats::default(), DurationStats::default());
                let (mut ticks, mut redrawn, mut skipped) = (0u32, 0u32, 0u32);
                let mut last_log = Instant::now();
                while !stop2.load(Ordering::Relaxed) {
                    let t0 = Instant::now();
                    tick_no += 1;
                    let inputs = TickInputs {
                        page: page.as_ref().and_then(|p| p.latest()),
                        cam: webcam.as_ref().and_then(|s| cam_fresh.latest(s)),
                        mask: mask.as_ref().and_then(|s| mask_fresh.latest(s)),
                        layout: *layout.read(),
                        backdrop: backdrop.read().clone(),
                    };
                    let note = ComposeNote::of(
                        &inputs.backdrop,
                        &inputs.layout,
                        inputs.cam.is_some(),
                        inputs.mask.is_some(),
                    );
                    stats_out.note.store(note_code(note), Ordering::Relaxed);
                    let preview_due = k.is_multiple_of(preview_every);
                    k = k.wrapping_add(1);
                    // Unchanged or failed: push the previous output again (same
                    // Arcs, no conversion), with its preview only when one is due.
                    let repeat = match slot.produce(&inputs, preview_due, prev.is_some()) {
                        Ok(Some(o)) => {
                            if is_redraw(prev.as_ref(), &o) {
                                redrawn += 1;
                            }
                            prev = Some(o);
                            false
                        }
                        // Warned (rate-limited) by the slot.
                        Ok(None) | Err(_) => true,
                    };
                    // After the produce call, so a fallback this tick counts at once.
                    pipe_frames.push_back(slot.producer.pipeline_frames());
                    if pipe_frames.len() > PIPELINE_WINDOW {
                        pipe_frames.pop_front();
                    }
                    pipeline_out.store(
                        median_pipeline_ms(pipe_frames.make_contiguous(), spec.fps),
                        Ordering::Relaxed,
                    );
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
                        let has_page = slot.producer.has_page(&inputs);
                        let pipeline = slot.producer.pipeline_frames();
                        match ready.pushed(tick_no, has_page, pipeline, Instant::now()) {
                            Some(ReadyWhy::Page) => {
                                tracing::info!(
                                    "first output with the page went out (output ready)"
                                );
                                stats_out.output_ready.store(true, Ordering::Release);
                            }
                            Some(ReadyWhy::Timeout) => {
                                tracing::warn!(
                                    "no page frame after 10 s; reporting the output ready anyway"
                                );
                                stats_out.output_ready.store(true, Ordering::Release);
                            }
                            None => {}
                        }
                    }
                    // Absolute deadlines avoid drift; if we fell behind, skip missed ticks.
                    n += 1;
                    ticks += 1;
                    stats_out.ticks.fetch_add(1, Ordering::Relaxed);
                    let now = Instant::now();
                    sinks_t.record(now - t1);
                    let mut deadline = start + period * n;
                    if deadline < now {
                        let next = (now.duration_since(start).as_secs_f64() / period.as_secs_f64())
                            as u32
                            + 1;
                        skipped += next - n;
                        stats_out
                            .skipped
                            .fetch_add(u64::from(next - n), Ordering::Relaxed);
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
                                "output ({}): compose {c}; sinks {s}",
                                slot.name
                            );
                        }
                        (ticks, redrawn, skipped) = (0, 0, 0);
                        last_log = Instant::now();
                    }
                    std::thread::sleep(deadline - now);
                }
            })
            .expect("spawn ticker thread");
        Ticker {
            stop,
            stats,
            pipeline_ms,
            handle,
            trigger,
        }
    }

    /// Running tick/skip totals and whether output has started.
    pub fn stats(&self) -> Arc<TickerStats> {
        self.stats.clone()
    }

    /// Time from the tick that composed a frame to the tick whose output
    /// carries it (median of the last ~60 ticks; 0 on the CPU path). The mic
    /// is delayed by this.
    pub fn pipeline_latency_ms(&self) -> u32 {
        self.pipeline_ms.load(Ordering::Relaxed)
    }

    /// The same value, shareable with other threads.
    pub fn pipeline_latency(&self) -> Arc<AtomicU32> {
        self.pipeline_ms.clone()
    }

    /// A handle that switches this ticker to the CPU compositor.
    pub fn fallback_trigger(&self) -> FallbackTrigger {
        self.trigger.clone()
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
            Some(page),
            webcam,
            mask,
            layout,
            Arc::default(),
            Box::new(CpuProducer::new(spec)),
            sinks,
            std::sync::mpsc::channel().0,
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
    fn pipeline_median_ignores_a_short_blip() {
        assert_eq!(median_pipeline_ms(&[], 30), 0);
        assert_eq!(median_pipeline_ms(&[2; 10], 30), 66);
        let mut v = vec![2u32; 40];
        v.extend([0; 5]);
        assert_eq!(median_pipeline_ms(&v, 30), 66);
        assert_eq!(median_pipeline_ms(&[0; 7], 30), 0);
    }

    #[test]
    fn ticker_reports_pipeline_latency_of_its_producer() {
        struct Lagging(Fake);
        impl OutputProducer for Lagging {
            fn produce(&mut self, i: &TickInputs, p: bool) -> anyhow::Result<Option<Output>> {
                self.0.produce(i, p)
            }
            fn name(&self) -> String {
                "Lagging".into()
            }
            fn pipeline_frames(&self) -> u32 {
                2
            }
        }
        let page = FrameSlot::new();
        page.put(Frame::new_black(64, 36));
        let (_s, t, _rx) = spawn_fake(Box::new(Lagging(Fake::new(|_| false))), page);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(t.pipeline_latency_ms(), 66);
        t.stop();
        // The CPU producer has none.
        let page = FrameSlot::new();
        page.put(Frame::new_black(64, 36));
        let (_s, t, _rx) = spawn_fake(Box::new(CpuProducer::new(small_spec())), page);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(t.pipeline_latency_ms(), 0);
        t.stop();
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
    fn every_note_survives_the_stats_cell() {
        let s = TickerStats::default();
        for n in [
            crate::ComposeNote::None,
            crate::ComposeNote::CutoutNotReady,
            crate::ComposeNote::WebcamOff,
        ] {
            s.note.store(note_code(n), Ordering::Relaxed);
            assert_eq!(s.compose_note(), n);
        }
    }

    #[test]
    fn ticker_reads_backdrop_live_and_reports_the_note() {
        // Blurred room without a mask: whole frame blurred (never the red
        // page) and "cut-out not ready"; back to the page when switched off.
        let cam = FrameSlot::new();
        cam.put(Frame::from_bgra(16, 9, GREEN.repeat(16 * 9)).unwrap());
        let backdrop = Arc::new(parking_lot::RwLock::new(crate::Backdrop {
            full: crate::FullBackdrop::BlurredRoom,
            ..crate::Backdrop::default()
        }));
        let samples: Samples = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let t = Ticker::spawn(
            small_spec(),
            Some(red_page()),
            Some(cam),
            Some(MaskSlot::new()),
            cutout_layout(),
            backdrop.clone(),
            Box::new(CpuProducer::new(small_spec())),
            vec![Box::new(Sampler(samples.clone(), 5, 5))],
            std::sync::mpsc::channel().0,
        );
        let stats = t.stats();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(stats.compose_note(), crate::ComposeNote::CutoutNotReady);
        assert_eq!(samples.lock().last().map(|s| s.1), Some(GREEN));
        *backdrop.write() = crate::Backdrop::default();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(stats.compose_note(), crate::ComposeNote::None);
        assert_eq!(samples.lock().last().map(|s| s.1), Some(RED));
        t.stop();
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
            backdrop: crate::Backdrop::default(),
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
    fn stats_count_ticks_and_mark_output_ready_once_the_page_painted() {
        let n = Arc::new(AtomicUsize::new(0));
        let page = FrameSlot::new();
        let t = spawn(
            SPEC,
            page.clone(),
            None,
            None,
            layout(),
            vec![Box::new(Counter(n.clone()))],
        );
        let stats = t.stats();
        std::thread::sleep(Duration::from_millis(500));
        // Frames go out (black, no page yet) but the tray must not switch to them.
        assert!(n.load(Ordering::SeqCst) > 0, "outputs pushed");
        assert!(!stats.output_ready(), "black frames are not ready");
        let (ticks, skipped) = (stats.ticks(), stats.skipped());
        assert!((10..=20).contains(&ticks), "ticks {ticks}");
        assert!(skipped <= 2, "skipped {skipped}");
        // The first paint (or test-pattern frame) makes it ready.
        page.put(Frame::new_black(64, 36));
        std::thread::sleep(Duration::from_millis(200));
        assert!(stats.output_ready(), "ready after the first page frame");
        t.stop();
        // A fresh stats object is not ready.
        assert!(!TickerStats::default().output_ready());
    }

    #[test]
    fn ready_gate_waits_for_the_page_through_the_pipeline_or_10s() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // CPU (no pipeline): ready on the tick the page is pushed.
        let mut g = ReadyGate::new(t0);
        assert_eq!(g.pushed(1, false, 0, ms(33)), None);
        assert_eq!(g.pushed(2, true, 0, ms(66)), Some(ReadyWhy::Page));
        assert_eq!(g.pushed(3, true, 0, ms(99)), None, "only once");
        // GPU readback ring (2 ticks): the page's frame leaves 2 ticks later.
        let mut g = ReadyGate::new(t0);
        assert_eq!(g.pushed(5, true, 2, ms(165)), None);
        assert_eq!(g.pushed(6, false, 2, ms(198)), None);
        assert_eq!(g.pushed(7, false, 2, ms(231)), Some(ReadyWhy::Page));
        // A page that never paints: ready after 10 s anyway.
        let mut g = ReadyGate::new(t0);
        assert_eq!(g.pushed(1, false, 0, ms(9_999)), None);
        assert_eq!(g.pushed(2, false, 0, ms(10_000)), Some(ReadyWhy::Timeout));
        assert_eq!(g.pushed(3, true, 0, ms(10_033)), None);
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

    /// A producer that draws a fixed BLUE frame and fails on the calls
    /// (1-based) `fail` picks, with a device-lost-like message.
    struct Fake {
        n: u32,
        fail: Box<dyn Fn(u32) -> bool + Send>,
        frame: Arc<Frame>,
        can_fall_back: bool,
    }

    impl Fake {
        fn new(fail: impl Fn(u32) -> bool + Send + 'static) -> Fake {
            Fake {
                n: 0,
                fail: Box::new(fail),
                frame: Arc::new(Frame::from_bgra(64, 36, BLUE.repeat(64 * 36)).unwrap()),
                can_fall_back: true,
            }
        }
    }

    impl OutputProducer for Fake {
        fn produce(&mut self, _: &TickInputs, _: bool) -> anyhow::Result<Option<Output>> {
            self.n += 1;
            if (self.fail)(self.n) {
                anyhow::bail!("fake device lost (call {})", self.n);
            }
            Ok(Some(Output {
                bgra: Some(self.frame.clone()),
                ..Output::default()
            }))
        }
        fn name(&self) -> String {
            "Fake".into()
        }
        fn can_fall_back(&self) -> bool {
            self.can_fall_back
        }
    }

    /// Runs `producer` over `page`; returns the samples at (10, 10), the
    /// ticker (still running) and the event receiver.
    fn spawn_fake(
        producer: Box<dyn OutputProducer>,
        page: FrameSlot,
    ) -> (Samples, Ticker, std::sync::mpsc::Receiver<ProducerEvent>) {
        let samples: Samples = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let t = Ticker::spawn(
            small_spec(),
            Some(page),
            None,
            None,
            layout(),
            Arc::default(),
            producer,
            vec![Box::new(Sampler(samples.clone(), 10, 10))],
            tx,
        );
        (samples, t, rx)
    }

    /// Largest gap between consecutive pushes.
    fn max_gap(s: &[(std::time::Instant, [u8; 4])]) -> Duration {
        s.windows(2)
            .map(|w| w[1].0.duration_since(w[0].0))
            .max()
            .unwrap_or_default()
    }

    #[test]
    fn producer_falls_back_after_repeated_device_loss() {
        let (samples, t, rx) = spawn_fake(Box::new(Fake::new(|n| n > 3)), red_page());
        let ev = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("no ProducerEvent");
        let at = std::time::Instant::now();
        let ProducerEvent::Failed(reason) = ev;
        assert!(reason.contains("fake device lost"), "{reason}");
        std::thread::sleep(Duration::from_millis(300));
        t.stop();
        let s = samples.lock();
        assert_eq!(s.first().map(|x| x.1), Some(BLUE), "producer frames first");
        let first_red = s
            .iter()
            .find(|(_, px)| *px == RED)
            .expect("no frame from the CPU fallback");
        assert!(
            first_red.0 <= at + Duration::from_millis(100),
            "fallback frame {:?} after the event",
            first_red.0.saturating_duration_since(at)
        );
        assert!(s.iter().skip_while(|x| x.1 != RED).all(|x| x.1 == RED));
        // The failing ticks re-pushed the previous output: no stall.
        assert!(max_gap(&s) < Duration::from_millis(80), "{:?}", max_gap(&s));
        assert!(rx.try_recv().is_err(), "more than one event");
    }

    #[test]
    fn producer_single_error_is_retried() {
        let (samples, t, rx) = spawn_fake(Box::new(Fake::new(|n| n == 3)), red_page());
        std::thread::sleep(Duration::from_millis(500));
        t.stop();
        assert!(rx.try_recv().is_err(), "fell back after one error");
        let s = samples.lock();
        assert!(s.len() >= 12, "{} pushes", s.len());
        assert!(s.iter().all(|x| x.1 == BLUE), "not the producer's frames");
        assert!(max_gap(&s) < Duration::from_millis(80));
    }

    #[test]
    fn non_consecutive_errors_do_not_fall_back() {
        let (_s, t, rx) = spawn_fake(Box::new(Fake::new(|n| n % 3 == 0)), red_page());
        std::thread::sleep(Duration::from_millis(400));
        t.stop();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn fallback_can_be_triggered_from_outside() {
        let (samples, t, rx) = spawn_fake(Box::new(Fake::new(|_| false)), red_page());
        std::thread::sleep(Duration::from_millis(150));
        t.fallback_trigger().fall_back("no page texture within 5 s");
        let at = std::time::Instant::now();
        let ProducerEvent::Failed(reason) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(reason, "no page texture within 5 s");
        // A second request does nothing.
        t.fallback_trigger().fall_back("again");
        std::thread::sleep(Duration::from_millis(200));
        t.stop();
        assert!(rx.try_recv().is_err());
        let s = samples.lock();
        let first_red = s.iter().find(|(_, px)| *px == RED).expect("no CPU frame");
        assert!(first_red.0 <= at + Duration::from_millis(100));
    }

    #[test]
    fn producer_that_cannot_fall_back_keeps_running() {
        // Like the CPU compositor itself: errors forever, nothing to switch to.
        let mut fake = Fake::new(|_| true);
        fake.can_fall_back = false;
        let (_s, t, rx) = spawn_fake(Box::new(fake), red_page());
        t.fallback_trigger().fall_back("ignored");
        std::thread::sleep(Duration::from_millis(200));
        t.stop();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn missing_cpu_page_holds_at_most_1s_then_shows_live_pip() {
        // The recreated browser never paints: after ~1 s the CPU compositor
        // draws a black page with the live webcam rather than freezing.
        const MAGENTA: [u8; 4] = [255, 0, 255, 255];
        let cam = FrameSlot::new();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (c, stop2) = (cam.clone(), stop.clone());
        let feeder = std::thread::spawn(move || {
            let mut i = 0u32;
            while !stop2.load(Ordering::SeqCst) {
                let px = if i.is_multiple_of(2) { GREEN } else { MAGENTA };
                c.put(Frame::from_bgra(16, 9, px.repeat(16 * 9)).unwrap());
                i += 1;
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        let samples: Samples = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let t = Ticker::spawn(
            small_spec(),
            Some(FrameSlot::new()),
            Some(cam),
            None,
            Arc::new(parking_lot::RwLock::new(crate::PipLayout::preset(
                crate::Corner::BottomRight,
                0.25,
            ))),
            Arc::default(),
            Box::new(Fake::new(|n| n > 3)),
            // PIP centre (the 16x9 PIP sits at (47, 26)).
            vec![Box::new(Sampler(samples.clone(), 55, 30))],
            tx,
        );
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let at = std::time::Instant::now();
        std::thread::sleep(Duration::from_millis(1600));
        t.stop();
        stop.store(true, Ordering::SeqCst);
        feeder.join().unwrap();
        let s = samples.lock();
        let first_live = s
            .iter()
            .find(|(t, px)| *t > at && *px != BLUE)
            .expect("still frozen on the last frame");
        assert!(
            first_live.0 <= at + Duration::from_millis(1200),
            "held {:?}",
            first_live.0.duration_since(at)
        );
        let late: Vec<_> = s
            .iter()
            .filter(|(t, _)| *t > at + Duration::from_millis(1250))
            .map(|x| x.1)
            .collect();
        assert!(
            late.contains(&GREEN) && late.contains(&MAGENTA),
            "PIP not live: {late:?}"
        );
    }

    /// Draws BLUE three times, then holds its output (`Ok(None)`, like a
    /// device rebuild) for 1 s and then fails for good (rebuild timed out).
    struct Holding {
        n: u32,
        since: Arc<parking_lot::Mutex<Option<std::time::Instant>>>,
        frame: Arc<Frame>,
    }

    impl OutputProducer for Holding {
        fn produce(&mut self, _: &TickInputs, _: bool) -> anyhow::Result<Option<Output>> {
            self.n += 1;
            if self.n <= 3 {
                return Ok(Some(Output {
                    bgra: Some(self.frame.clone()),
                    ..Output::default()
                }));
            }
            let since = *self
                .since
                .lock()
                .get_or_insert_with(std::time::Instant::now);
            if since.elapsed() < Duration::from_secs(1) {
                return Ok(None);
            }
            anyhow::bail!("rebuild timed out")
        }
        fn name(&self) -> String {
            "Holding".into()
        }
        fn holding_since(&self) -> Option<std::time::Instant> {
            *self.since.lock()
        }
    }

    #[test]
    fn rebuild_timeout_then_missing_page_freezes_at_most_1_2s_in_all() {
        const MAGENTA: [u8; 4] = [255, 0, 255, 255];
        let cam = FrameSlot::new();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (c, stop2) = (cam.clone(), stop.clone());
        let feeder = std::thread::spawn(move || {
            let mut i = 0u32;
            while !stop2.load(Ordering::SeqCst) {
                let px = if i.is_multiple_of(2) { GREEN } else { MAGENTA };
                c.put(Frame::from_bgra(16, 9, px.repeat(16 * 9)).unwrap());
                i += 1;
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        let since = Arc::new(parking_lot::Mutex::new(None));
        let samples: Samples = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let t = Ticker::spawn(
            small_spec(),
            Some(FrameSlot::new()),
            Some(cam),
            None,
            Arc::new(parking_lot::RwLock::new(crate::PipLayout::preset(
                crate::Corner::BottomRight,
                0.25,
            ))),
            Arc::default(),
            Box::new(Holding {
                n: 0,
                since: since.clone(),
                frame: Arc::new(Frame::from_bgra(64, 36, BLUE.repeat(64 * 36)).unwrap()),
            }),
            vec![Box::new(Sampler(samples.clone(), 55, 30))],
            tx,
        );
        rx.recv_timeout(Duration::from_secs(3)).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        t.stop();
        stop.store(true, Ordering::SeqCst);
        feeder.join().unwrap();
        let held_from = since.lock().expect("never held");
        let s = samples.lock();
        let first_live = s
            .iter()
            .find(|(t, px)| *t > held_from && *px != BLUE)
            .expect("still frozen on the last frame");
        let froze = first_live.0.duration_since(held_from);
        assert!(froze <= Duration::from_millis(1200), "froze {froze:?}");
    }

    #[test]
    fn keeps_last_frame_until_the_cpu_page_arrives() {
        // After a fallback with no CPU page yet (the browser is being
        // recreated), keep showing the last frame instead of black.
        let page = FrameSlot::new();
        let (samples, t, rx) = spawn_fake(Box::new(Fake::new(|n| n > 3)), page.clone());
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            samples.lock().iter().all(|x| x.1 == BLUE),
            "black before the CPU page arrived"
        );
        page.put(Frame::from_bgra(64, 36, RED.repeat(64 * 36)).unwrap());
        std::thread::sleep(Duration::from_millis(150));
        t.stop();
        assert_eq!(samples.lock().last().map(|x| x.1), Some(RED));
    }

    #[test]
    fn unchanged_tick_allocates_nothing_when_pool_is_held() {
        let mut p = CpuProducer::new(small_spec());
        let mut held = Vec::new();
        let mut last = None;
        // Sinks hold every pool frame.
        for i in 0..POOL {
            let px = [i as u8, 0, 0, 255];
            let page = Arc::new(Frame::from_bgra(64, 36, px.repeat(64 * 36)).unwrap());
            let inputs = TickInputs {
                page: Some(page),
                cam: None,
                mask: None,
                layout: crate::PipLayout::default(),
                backdrop: crate::Backdrop::default(),
            };
            held.push(p.produce(&inputs, false).unwrap().unwrap());
            last = Some(inputs);
        }
        let pool: Vec<_> = p.pool.clone();
        assert_eq!(pool.len(), POOL);
        assert!(p.produce(last.as_ref().unwrap(), false).unwrap().is_none());
        assert!(
            p.pool.iter().zip(&pool).all(|(a, b)| Arc::ptr_eq(a, b)),
            "a pool frame was replaced on an unchanged tick"
        );
    }

    #[test]
    fn replayed_output_is_not_a_redraw() {
        let f = Arc::new(Frame::new_black(4, 4));
        let a = Output {
            bgra: Some(f.clone()),
            ..Output::default()
        };
        let with_preview = Output {
            preview: Some(f.clone()),
            ..a.clone()
        };
        assert!(is_redraw(None, &a));
        assert!(!is_redraw(Some(&a), &with_preview));
        let b = Output {
            bgra: Some(Arc::new(Frame::new_black(4, 4))),
            ..Output::default()
        };
        assert!(is_redraw(Some(&a), &b));
        let n = Arc::new(p2c_core::bgra_to_nv12(&Frame::new_black(4, 4), 4, 4));
        let c = Output {
            nv12: Some(n.clone()),
            ..Output::default()
        };
        let d = Output {
            nv12: Some(n),
            preview: Some(f),
            ..Output::default()
        };
        assert!(!is_redraw(Some(&c), &d));
    }
}
