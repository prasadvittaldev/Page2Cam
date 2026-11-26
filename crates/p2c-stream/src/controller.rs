use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;
use p2c_audio::Mixer;
use p2c_compositor::{FrameSink, Output};
use p2c_core::{Frame, Nv12Frame, Slot};
use parking_lot::Mutex;

use crate::{encoder_args, redact_all, Encoder, StreamConfig};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamState {
    Idle,
    Connecting,
    Live,
    Error(String),
}

impl std::fmt::Display for StreamState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamState::Idle => write!(f, "idle"),
            StreamState::Connecting => write!(f, "connecting"),
            StreamState::Live => write!(f, "live"),
            StreamState::Error(e) => write!(f, "error: {e}"),
        }
    }
}

/// A frame for the video writer, shared with the ticker (no copy).
pub(crate) enum VideoFrame {
    Bgra(Arc<Frame>),
    Nv12(Arc<Nv12Frame>),
}

impl VideoFrame {
    /// The raw bytes, if this frame is `pix_fmt` at `w`x`h`.
    fn bytes(&self, pix_fmt: &str, w: u32, h: u32) -> Option<&[u8]> {
        match self {
            VideoFrame::Bgra(f) if pix_fmt == "bgra" && (f.width, f.height) == (w, h) => {
                Some(&f.data)
            }
            VideoFrame::Nv12(f) if pix_fmt == "nv12" && (f.width, f.height) == (w, h) => {
                Some(&f.data)
            }
            _ => None,
        }
    }
}

pub(crate) type VideoSlot = Slot<VideoFrame>;

/// A black frame in `pix_fmt` (NV12: Y 16, UV 128; BGRA: opaque black).
fn black_frame(pix_fmt: &str, w: u32, h: u32) -> Vec<u8> {
    if pix_fmt == "nv12" {
        let luma = w as usize * h as usize;
        let mut v = vec![16u8; luma + luma / 2];
        v[luma..].fill(128);
        v
    } else {
        Frame::new_black(w, h).data
    }
}

/// Ticker-side sink: hands frames to the stream writer without blocking.
/// Takes `out.nv12` when present, else `out.bgra`.
pub struct StreamSink {
    slot: VideoSlot,
    active: Arc<AtomicBool>,
}

impl FrameSink for StreamSink {
    fn push(&mut self, out: &Output) -> anyhow::Result<()> {
        if self.active.load(Ordering::Relaxed) {
            if let Some(f) = &out.nv12 {
                self.slot.put(VideoFrame::Nv12(f.clone()));
            } else if let Some(f) = &out.bgra {
                self.slot.put(VideoFrame::Bgra(f.clone()));
            }
        }
        Ok(())
    }
}

struct Running {
    child: Arc<Mutex<Child>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    /// What it was started with, for a retry with x264.
    cfg: StreamConfig,
    ffmpeg: PathBuf,
    /// Which start this is (a retry only replaces the run that failed).
    generation: u64,
}

/// Starts/stops one ffmpeg RTMP publish at a time.
pub struct StreamController {
    inner: Arc<Inner>,
}

struct Inner {
    mixer: Mixer,
    slot: VideoSlot,
    active: Arc<AtomicBool>,
    state: Arc<Mutex<StreamState>>,
    running: Mutex<Option<Running>>,
    /// Used by the next `start` (a live stream keeps the one it began with).
    encoder: Mutex<Encoder>,
    /// Set when a hardware encoder failed at stream start: x264 from then on
    /// (for this session), and why.
    x264_pinned: Mutex<Option<String>>,
    generation: AtomicU64,
}

impl StreamController {
    pub fn new(mixer: Mixer) -> Self {
        StreamController {
            inner: Arc::new(Inner {
                mixer,
                slot: VideoSlot::new(),
                active: Arc::new(AtomicBool::new(false)),
                state: Arc::new(Mutex::new(StreamState::Idle)),
                running: Mutex::new(None),
                encoder: Mutex::new(Encoder::X264),
                x264_pinned: Mutex::new(None),
                generation: AtomicU64::new(0),
            }),
        }
    }

    pub fn sink(&self) -> StreamSink {
        StreamSink {
            slot: self.inner.slot.clone(),
            active: self.inner.active.clone(),
        }
    }

    /// Encoder for the next stream start; a stream already live is not
    /// restarted. Ignored once a hardware encoder failed at stream start
    /// (x264 is kept for the session).
    pub fn set_encoder(&self, e: Encoder) {
        if self.inner.x264_pinned.lock().is_none() {
            *self.inner.encoder.lock() = e;
        }
    }

    /// Why the stream switched to x264 for this session (a hardware encoder
    /// failed to open at stream start), if it did.
    pub fn encoder_fallback(&self) -> Option<String> {
        self.inner.x264_pinned.lock().clone()
    }

    pub fn state(&self) -> StreamState {
        self.inner.state.lock().clone()
    }

    pub fn start(&self, cfg: StreamConfig, ffmpeg: &Path) -> anyhow::Result<()> {
        self.stop();
        let mut running = self.inner.running.lock();
        match Inner::launch(&self.inner, cfg, ffmpeg) {
            Ok(r) => {
                *running = Some(r);
                Ok(())
            }
            Err(e) => {
                *self.inner.state.lock() = StreamState::Error(format!("{e:#}"));
                Err(e)
            }
        }
    }

    pub fn stop(&self) {
        let Some(r) = self.inner.running.lock().take() else {
            return;
        };
        self.inner.halt(r);
        *self.inner.state.lock() = StreamState::Idle;
    }
}

impl Inner {
    /// Start ffmpeg and its writer threads (the caller stores the result).
    fn launch(inner: &Arc<Inner>, mut cfg: StreamConfig, ffmpeg: &Path) -> anyhow::Result<Running> {
        let dest = cfg.destinations.first().context("no stream destination")?;
        anyhow::ensure!(!dest.url.trim().is_empty(), "enter the stream server URL");
        let target = dest.target();
        let key = dest.key.clone();
        let listener = TcpListener::bind("127.0.0.1:0").context("opening the audio socket")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        cfg.encoder = *inner.encoder.lock();
        // Until the encoder/sender split (Task 3): one process publishes to the first
        // destination, so swap the encoder's MPEG-TS output for FLV to RTMP.
        let mut args = encoder_args(&cfg, port);
        args.truncate(args.len() - 5);
        args.extend(["-f".to_string(), "flv".to_string(), target]);
        tracing::info!("starting ffmpeg: {}", redact_all(&args.join(" "), &[&key]));
        let mut cmd = Command::new(ffmpeg);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().with_context(|| {
            format!(
                "could not start ffmpeg at {} (is ffmpeg installed?)",
                ffmpeg.display()
            )
        })?;
        let stdin = child.stdin.take().context("ffmpeg stdin")?;
        let stderr = child.stderr.take().context("ffmpeg stderr")?;
        let child = Arc::new(Mutex::new(child));
        let stop = Arc::new(AtomicBool::new(false));
        let generation = inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *inner.state.lock() = StreamState::Connecting;
        inner.mixer.reset();
        inner.active.store(true, Ordering::Relaxed);
        // One timeline for both writers: video frame 0 and audio sample 0 = t0.
        let t0 = Instant::now();

        let mut threads = Vec::new();
        // Audio time to leave out after video drops a backlog (in samples).
        let audio_skip = Arc::new(AtomicU64::new(0));
        {
            let skip = audio_skip.clone();
            let (slot, stop, spec) = (
                inner.slot.clone(),
                stop.clone(),
                (cfg.width, cfg.height, cfg.fps, cfg.pix_fmt),
            );
            threads.push(thread("p2c-stream-video", move || {
                write_video(stdin, slot, stop, spec, t0, skip)
            }));
        }
        {
            let (mixer, stop, skip) = (inner.mixer.clone(), stop.clone(), audio_skip.clone());
            threads.push(thread("p2c-stream-audio", move || {
                write_audio(listener, mixer, stop, t0, skip)
            }));
        }
        {
            let (state, stop, key) = (inner.state.clone(), stop.clone(), key);
            let (weak, encoder) = (Arc::downgrade(inner), cfg.encoder);
            threads.push(thread("p2c-stream-log", move || {
                let lines = watch_stderr(stderr, &state, &key);
                ended(&lines, &state, &stop, encoder, weak, generation);
            }));
        }
        Ok(Running {
            child,
            stop,
            threads,
            cfg,
            ffmpeg: ffmpeg.to_path_buf(),
            generation,
        })
    }

    /// Stop a run: its threads end and ffmpeg is killed.
    fn halt(&self, r: Running) {
        r.stop.store(true, Ordering::SeqCst);
        self.active.store(false, Ordering::Relaxed);
        {
            let mut c = r.child.lock();
            let _ = c.kill();
            let _ = c.wait();
        }
        for t in r.threads {
            let _ = t.join();
        }
    }

    /// The hardware encoder of run `generation` failed to open: start the
    /// same stream again with x264 and keep x264 for the session. Nothing
    /// happens if the stream was stopped or restarted meanwhile.
    fn retry_with_x264(self: Arc<Inner>, generation: u64, reason: String) {
        let mut running = self.running.lock();
        let Some(old) = running.take_if(|r| r.generation == generation) else {
            return;
        };
        let (cfg, ffmpeg) = (old.cfg.clone(), old.ffmpeg.clone());
        self.halt(old);
        tracing::warn!("{reason}; streaming with x264 from now on");
        *self.x264_pinned.lock() = Some(reason);
        *self.encoder.lock() = Encoder::X264;
        match Inner::launch(&self, cfg, &ffmpeg) {
            Ok(r) => *running = Some(r),
            Err(e) => *self.state.lock() = StreamState::Error(format!("{e:#}")),
        }
    }
}

impl Drop for StreamController {
    fn drop(&mut self) {
        self.stop();
    }
}

fn thread(name: &str, f: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("spawn stream thread")
}

/// Write exactly `fps` frames per second of wall-clock time since `t0`
/// (repeating the latest frame when the compositor is slower). After a stall
/// it catches up with repeated frames instead of restarting its clock, so
/// video and audio (paced from the same `t0`) stay on one timeline.
fn write_video(
    mut stdin: impl Write,
    slot: VideoSlot,
    stop: Arc<AtomicBool>,
    (w, h, fps, pix_fmt): (u32, u32, u32, &str),
    t0: Instant,
    audio_skip: Arc<AtomicU64>,
) {
    let fps = fps.max(1);
    let period = Duration::from_secs_f64(1.0 / fps as f64);
    let black = black_frame(pix_fmt, w, h);
    let mut n: u64 = 0;
    let mut last_warn: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        // Frames that should exist by now (one at t0, then one per period).
        let due = (t0.elapsed().as_secs_f64() / period.as_secs_f64()) as u64 + 1;
        if due > n + MAX_LAG_SECS * fps as u64 {
            // The encoder can't keep up: drop the backlog instead of bursting
            // it, and make audio drop the same time so both stay in sync.
            let dropped = due - n - 1;
            n += dropped;
            audio_skip.fetch_add(
                dropped * p2c_core::SAMPLE_RATE as u64 / fps as u64,
                Ordering::Relaxed,
            );
            if last_warn.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
                tracing::warn!(
                    dropped_frames = dropped,
                    "the video encoder can't keep up; lower the bitrate or resolution"
                );
                last_warn = Some(Instant::now());
            }
        }
        // Catch up one frame at a time, always with the newest picture.
        while n < due && !stop.load(Ordering::Relaxed) {
            let latest = slot.latest();
            let f = latest
                .as_deref()
                .and_then(|f| f.bytes(pix_fmt, w, h))
                .unwrap_or(&black);
            if stdin.write_all(f).is_err() {
                return;
            }
            n += 1;
        }
        let deadline = t0 + period.mul_f64(n as f64);
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        }
    }
}

/// How far video may fall behind the wall clock before the backlog is dropped.
const MAX_LAG_SECS: u64 = 2;

/// Stereo frames of silence that put audio on the video's timeline when
/// ffmpeg connects `lead` after video started.
fn lead_in_frames(lead: Duration) -> usize {
    (lead.as_secs_f64() * p2c_core::SAMPLE_RATE as f64).round() as usize
}

/// Accept ffmpeg's audio connection, then send 10 ms of mixed audio per tick.
fn write_audio(
    listener: TcpListener,
    mixer: Mixer,
    stop: Arc<AtomicBool>,
    t0: Instant,
    skip: Arc<AtomicU64>,
) {
    let accept_deadline = Instant::now() + Duration::from_secs(20);
    let mut sock = loop {
        if stop.load(Ordering::Relaxed) || Instant::now() > accept_deadline {
            return;
        }
        match listener.accept() {
            Ok((s, _)) => break s,
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let _ = sock.set_nonblocking(false);
    let _ = sock.set_nodelay(true);
    const FRAMES: usize = 480; // 10 ms at 48 kHz
    let period = Duration::from_millis(10);
    // Video started at t0; fill the time before ffmpeg connected with silence
    // so audio sample 0 lines up with video frame 0.
    let lead = lead_in_frames(t0.elapsed());
    let silence = vec![0u8; lead * p2c_core::CHANNELS as usize * 4];
    if sock.write_all(&silence).is_err() {
        return;
    }
    let start = t0 + Duration::from_secs_f64(lead as f64 / p2c_core::SAMPLE_RATE as f64);
    let mut n: u32 = 0;
    let mut bytes = Vec::with_capacity(FRAMES * 8);
    let mut skip_rest: u64 = 0;
    while !stop.load(Ordering::Relaxed) {
        // Video dropped a backlog: leave out the same stretch of audio (its
        // 10 ms chunks are simply not sent, and the mixed audio is discarded).
        skip_rest += skip.swap(0, Ordering::Relaxed);
        if skip_rest >= FRAMES as u64 {
            let chunks = skip_rest / FRAMES as u64;
            skip_rest %= FRAMES as u64;
            for _ in 0..chunks {
                let _ = mixer.pull(FRAMES);
            }
            n += chunks as u32;
        }
        bytes.clear();
        for s in mixer.pull(FRAMES) {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        if sock.write_all(&bytes).is_err() {
            break;
        }
        n += 1;
        let deadline = start + period * n;
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        }
    }
}

/// Track ffmpeg's progress: "frame=" stats mean we're live; exit means error.
/// Returns ffmpeg's last log lines (redacted) once it has exited.
fn watch_stderr(
    stderr: std::process::ChildStderr,
    state: &Mutex<StreamState>,
    key: &str,
) -> Vec<String> {
    const KEEP: usize = 20;
    let mut reader = BufReader::new(stderr);
    let mut lines = std::collections::VecDeque::with_capacity(KEEP + 1);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // Stats lines end in '\r', log lines in '\n'.
        let n = read_until_any(&mut reader, &mut buf);
        if n == 0 {
            break;
        }
        let line = redact_all(String::from_utf8_lossy(&buf).trim(), &[key]);
        if line.is_empty() {
            continue;
        }
        if line.starts_with("frame=") || line.contains(" frame=") {
            let mut s = state.lock();
            if *s == StreamState::Connecting {
                tracing::info!("stream is live");
                *s = StreamState::Live;
            }
        } else {
            tracing::debug!(target: "ffmpeg", "{line}");
            lines.push_back(line);
            if lines.len() > KEEP {
                lines.pop_front();
            }
        }
    }
    lines.into()
}

/// ffmpeg exited (stderr closed). Unless we stopped it: a hardware encoder
/// that failed to open before the stream went live is retried once with
/// x264 (on another thread: the retry joins this one); anything else is
/// shown as the stream's error.
fn ended(
    lines: &[String],
    state: &Mutex<StreamState>,
    stop: &AtomicBool,
    encoder: Encoder,
    inner: std::sync::Weak<Inner>,
    generation: u64,
) {
    if stop.load(Ordering::SeqCst) {
        return;
    }
    let connecting = *state.lock() == StreamState::Connecting;
    if crate::retry_with_x264(encoder, connecting, lines) {
        if let Some(inner) = inner.upgrade() {
            let detail = lines.last().map(String::as_str).unwrap_or_default();
            tracing::warn!(%encoder, "the {encoder} encoder failed to open: {detail}");
            let reason = format!("{encoder} failed at stream start");
            let spawned = std::thread::Builder::new()
                .name("p2c-stream-retry".into())
                .spawn(move || inner.retry_with_x264(generation, reason));
            if spawned.is_ok() {
                return;
            }
        }
    }
    let why = lines
        .last()
        .cloned()
        .unwrap_or_else(|| "ffmpeg exited".to_string());
    tracing::warn!("stream ended: {why}");
    *state.lock() = StreamState::Error(why);
}

fn read_until_any(r: &mut impl BufRead, out: &mut Vec<u8>) -> usize {
    let mut total = 0;
    let mut byte = [0u8; 1];
    loop {
        match r.read(&mut byte) {
            Ok(0) | Err(_) => return total,
            Ok(_) => {
                total += 1;
                if byte[0] == b'\n' || byte[0] == b'\r' {
                    return total;
                }
                out.push(byte[0]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts frames written; blocks once (simulating an ffmpeg stall).
    struct StallingSink {
        frames: Arc<std::sync::atomic::AtomicUsize>,
        frame_len: usize,
        buf: usize,
        stall_at: usize,
        stall: Duration,
    }

    impl Write for StallingSink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.buf += b.len();
            while self.buf >= self.frame_len {
                self.buf -= self.frame_len;
                let n = self.frames.fetch_add(1, Ordering::SeqCst) + 1;
                if n == self.stall_at {
                    std::thread::sleep(self.stall);
                }
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn video_stays_on_the_wall_clock_after_a_stall() {
        // Audio is paced from the same start; video must catch up after a 1.5 s
        // stall instead of restarting its clock, or the sound ends up late.
        let frames = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sink = StallingSink {
            frames: frames.clone(),
            frame_len: 4 * 4 * 4,
            buf: 0,
            stall_at: 10,
            stall: Duration::from_millis(1500),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let t0 = Instant::now();
        let h = std::thread::spawn(move || {
            let skip = Arc::new(AtomicU64::new(0));
            write_video(sink, VideoSlot::new(), stop2, (4, 4, 30, "bgra"), t0, skip)
        });
        std::thread::sleep(Duration::from_millis(3000));
        stop.store(true, Ordering::SeqCst);
        h.join().unwrap();
        let n = frames.load(Ordering::SeqCst) as i64;
        // 3 s at 30 fps = 90 frames (+1 for the frame at t=0).
        assert!((n - 91).abs() <= 3, "wrote {n} frames in 3 s");
    }

    /// Records the first byte of every frame written; each write takes `per_frame`.
    struct SlowSink {
        seen: Arc<parking_lot::Mutex<Vec<u8>>>,
        frame_len: usize,
        buf: Vec<u8>,
        per_frame: Duration,
    }

    impl Write for SlowSink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.buf.extend_from_slice(b);
            while self.buf.len() >= self.frame_len {
                let first = self.buf[0];
                self.buf.drain(..self.frame_len);
                self.seen.lock().push(first);
                std::thread::sleep(self.per_frame);
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn slow_encoder_never_freezes_the_picture() {
        // Encoder manages 20 fps of a 30 fps stream. The picture must keep
        // changing (no long runs of one repeated frame), and once video is
        // more than 2 s behind both writers drop the backlog together.
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let sink = SlowSink {
            seen: seen.clone(),
            frame_len: 4 * 4 * 4,
            buf: Vec::new(),
            per_frame: Duration::from_millis(50),
        };
        let slot = VideoSlot::new();
        let stop = Arc::new(AtomicBool::new(false));
        let feeder = {
            let (slot, stop) = (slot.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut v = 0u8;
                while !stop.load(Ordering::SeqCst) {
                    v = v.wrapping_add(1);
                    slot.put(VideoFrame::Bgra(Arc::new(
                        Frame::from_bgra(4, 4, vec![v; 64]).unwrap(),
                    )));
                    std::thread::sleep(Duration::from_millis(33));
                }
            })
        };
        let skip = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (stop2, skip2) = (stop.clone(), skip.clone());
        let t0 = Instant::now();
        let h = std::thread::spawn(move || {
            write_video(sink, slot, stop2, (4, 4, 30, "bgra"), t0, skip2)
        });
        std::thread::sleep(Duration::from_millis(9000));
        stop.store(true, Ordering::SeqCst);
        h.join().unwrap();
        feeder.join().unwrap();
        let seen = seen.lock();
        let longest = seen
            .chunk_by(|a, b| a == b)
            .map(|run| run.len())
            .max()
            .unwrap_or(0);
        assert!(longest <= 3, "picture froze for {longest} frames");
        assert!(
            skip.load(Ordering::SeqCst) > 0,
            "audio was never told to drop the backlog"
        );
    }

    /// Collects everything written.
    struct Bytes(Arc<parking_lot::Mutex<Vec<u8>>>);
    impl Write for Bytes {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Runs the video writer for ~100 ms and returns what it wrote.
    fn write_for_a_bit(slot: VideoSlot, pix_fmt: &'static str) -> Vec<u8> {
        let got = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let sink = Bytes(got.clone());
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let h = std::thread::spawn(move || {
            let skip = Arc::new(AtomicU64::new(0));
            write_video(sink, slot, stop2, (4, 4, 30, pix_fmt), Instant::now(), skip)
        });
        std::thread::sleep(Duration::from_millis(100));
        stop.store(true, Ordering::SeqCst);
        h.join().unwrap();
        let v = got.lock().clone();
        v
    }

    #[test]
    fn nv12_stream_writes_nv12_frames() {
        let mut sink = StreamSink {
            slot: VideoSlot::new(),
            active: Arc::new(AtomicBool::new(true)),
        };
        let nv12 = Arc::new(p2c_core::Nv12Frame::new(4, 4, vec![77; 24]).unwrap());
        sink.push(&Output {
            nv12: Some(nv12),
            ..Output::default()
        })
        .unwrap();
        let got = write_for_a_bit(sink.slot.clone(), "nv12");
        assert!(
            got.len() >= 24 && got.len().is_multiple_of(24),
            "{}",
            got.len()
        );
        assert!(got.iter().all(|&b| b == 77));
        // No frame yet: NV12 black (Y 16, UV 128), not BGRA zeros.
        let got = write_for_a_bit(VideoSlot::new(), "nv12");
        assert!(!got.is_empty() && got.len().is_multiple_of(24));
        assert_eq!(
            &got[..24],
            &[[16u8; 16].as_slice(), &[128u8; 8]].concat()[..]
        );
    }

    #[test]
    fn bgra_stream_takes_bgra_from_output() {
        let mut sink = StreamSink {
            slot: VideoSlot::new(),
            active: Arc::new(AtomicBool::new(true)),
        };
        sink.push(&Output {
            bgra: Some(Arc::new(Frame::from_bgra(4, 4, vec![9; 64]).unwrap())),
            ..Output::default()
        })
        .unwrap();
        let got = write_for_a_bit(sink.slot.clone(), "bgra");
        assert!(got.len() >= 64 && got.len().is_multiple_of(64));
        assert!(got.iter().all(|&b| b == 9));
    }

    #[test]
    fn audio_lead_in_matches_time_since_video_start() {
        assert_eq!(lead_in_frames(Duration::from_millis(250)), 12_000);
        assert_eq!(lead_in_frames(Duration::ZERO), 0);
    }

    #[test]
    fn missing_ffmpeg_reports_error() {
        let c = StreamController::new(p2c_audio::Mixer::new(
            p2c_core::AudioRing::new(100),
            p2c_core::AudioRing::new(100),
            p2c_audio::MixSettings::default(),
        ));
        let cfg = crate::StreamConfig {
            destinations: vec![crate::Destination {
                site: crate::Site::YouTube,
                url: "rtmp://127.0.0.1:1/live".into(),
                key: "k".into(),
            }],
            width: 64,
            height: 36,
            fps: 30,
            video_kbps: 500,
            pix_fmt: "bgra",
            encoder: crate::Encoder::X264,
        };
        let err = c
            .start(cfg, std::path::Path::new("/nonexistent/ffmpeg"))
            .unwrap_err();
        assert!(format!("{err:#}").contains("ffmpeg"), "{err:#}");
        assert!(matches!(c.state(), StreamState::Error(_)));
    }

    /// A fake ffmpeg that appends its arguments to `log`, then fails: with
    /// "No capable devices found" for h264_nvenc, else with a network error.
    fn fake_ffmpeg(dir: &std::path::Path, log: &std::path::Path) -> std::path::PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("ffmpeg.cmd");
            let script = format!(
                "@echo off\r\n\
                 echo %* >> \"{}\"\r\n\
                 echo %* | findstr /c:\"h264_nvenc\" >nul\r\n\
                 if not errorlevel 1 (\r\n\
                 echo [h264_nvenc @ 0001] No capable devices found 1>&2\r\n\
                 echo Error while opening encoder for output stream #0:0 1>&2\r\n\
                 exit /b 1\r\n\
                 )\r\n\
                 echo [tcp @ 0001] Connection to tcp://127.0.0.1:1 failed: Connection refused 1>&2\r\n\
                 exit /b 1\r\n",
                log.display()
            );
            std::fs::write(&p, script).unwrap();
            p
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join("ffmpeg");
            let script = format!(
                "#!/bin/sh\n\
                 echo \"$*\" >> '{}'\n\
                 case \"$*\" in *h264_nvenc*)\n\
                 echo '[h264_nvenc @ 0x1] No capable devices found' >&2\n\
                 echo 'Error while opening encoder for output stream #0:0' >&2\n\
                 exit 1;;\n\
                 esac\n\
                 echo '[tcp @ 0x1] Connection to tcp://127.0.0.1:1 failed: Connection refused' >&2\n\
                 exit 1\n",
                log.display()
            );
            std::fs::write(&p, script).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    fn wait_for_error(c: &StreamController) -> String {
        let start = Instant::now();
        loop {
            if let StreamState::Error(e) = c.state() {
                return e;
            }
            assert!(start.elapsed() < Duration::from_secs(15), "{}", c.state());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn hardware_encoder_failing_at_start_is_retried_once_with_x264() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args.log");
        let ffmpeg = fake_ffmpeg(dir.path(), &log);
        let c = StreamController::new(p2c_audio::Mixer::new(
            p2c_core::AudioRing::new(100),
            p2c_core::AudioRing::new(100),
            p2c_audio::MixSettings::default(),
        ));
        let cfg = crate::StreamConfig {
            destinations: vec![crate::Destination {
                site: crate::Site::YouTube,
                url: "rtmp://127.0.0.1:1/live".into(),
                key: "k".into(),
            }],
            width: 64,
            height: 36,
            fps: 30,
            video_kbps: 500,
            pix_fmt: "bgra",
            encoder: Encoder::X264,
        };
        let runs = || -> Vec<String> {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect()
        };
        c.set_encoder(Encoder::Nvenc);
        assert_eq!(c.encoder_fallback(), None);
        c.start(cfg.clone(), &ffmpeg).unwrap();
        // NVENC fails to open -> x264 -> fails on the network: shown as is.
        let err = wait_for_error(&c);
        assert!(err.contains("Connection refused"), "{err}");
        let r = runs();
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(r[0].contains("h264_nvenc"), "{}", r[0]);
        assert!(r[1].contains("libx264"), "{}", r[1]);
        let why = c.encoder_fallback().expect("x264 pinned");
        assert!(why.contains("NVENC"), "{why}");
        // Pinned for the session: the probe's late answer doesn't undo it.
        c.set_encoder(Encoder::Nvenc);
        c.start(cfg, &ffmpeg).unwrap();
        wait_for_error(&c);
        let r = runs();
        assert_eq!(r.len(), 3, "{r:?}");
        assert!(r[2].contains("libx264"), "{}", r[2]);
        c.stop();
    }

    #[test]
    fn idle_until_started() {
        let c = StreamController::new(p2c_audio::Mixer::new(
            p2c_core::AudioRing::new(100),
            p2c_core::AudioRing::new(100),
            p2c_audio::MixSettings::default(),
        ));
        assert_eq!(c.state(), StreamState::Idle);
        assert_eq!(c.state().to_string(), "idle");
    }
}
