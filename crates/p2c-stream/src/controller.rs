use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;
use p2c_audio::Mixer;
use p2c_compositor::FrameSink;
use p2c_core::{Frame, FrameSlot};
use parking_lot::Mutex;

use crate::{ffmpeg_args, redact, StreamConfig};

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

/// Ticker-side sink: hands frames to the stream writer without blocking.
pub struct StreamSink {
    slot: FrameSlot,
    active: Arc<AtomicBool>,
}

impl FrameSink for StreamSink {
    fn push(&mut self, frame: &Frame) -> anyhow::Result<()> {
        if self.active.load(Ordering::Relaxed) {
            self.slot.put(frame.clone());
        }
        Ok(())
    }
}

struct Running {
    child: Arc<Mutex<Child>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

/// Starts/stops one ffmpeg RTMP publish at a time.
pub struct StreamController {
    mixer: Mixer,
    slot: FrameSlot,
    active: Arc<AtomicBool>,
    state: Arc<Mutex<StreamState>>,
    running: Mutex<Option<Running>>,
}

impl StreamController {
    pub fn new(mixer: Mixer) -> Self {
        StreamController {
            mixer,
            slot: FrameSlot::new(),
            active: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(StreamState::Idle)),
            running: Mutex::new(None),
        }
    }

    pub fn sink(&self) -> StreamSink {
        StreamSink {
            slot: self.slot.clone(),
            active: self.active.clone(),
        }
    }

    pub fn state(&self) -> StreamState {
        self.state.lock().clone()
    }

    pub fn start(&self, cfg: StreamConfig, ffmpeg: &Path) -> anyhow::Result<()> {
        self.stop();
        let result = self.spawn(cfg, ffmpeg);
        if let Err(e) = &result {
            *self.state.lock() = StreamState::Error(format!("{e:#}"));
        }
        result
    }

    fn spawn(&self, cfg: StreamConfig, ffmpeg: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(!cfg.url.trim().is_empty(), "enter the stream server URL");
        let listener = TcpListener::bind("127.0.0.1:0").context("opening the audio socket")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let args = ffmpeg_args(&cfg, port);
        tracing::info!("starting ffmpeg: {}", redact(&args.join(" "), &cfg.key));
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
        *self.state.lock() = StreamState::Connecting;
        self.mixer.reset();
        self.active.store(true, Ordering::Relaxed);
        // One timeline for both writers: video frame 0 and audio sample 0 = t0.
        let t0 = Instant::now();

        let mut threads = Vec::new();
        // Audio time to leave out after video drops a backlog (in samples).
        let audio_skip = Arc::new(AtomicU64::new(0));
        {
            let skip = audio_skip.clone();
            let (slot, stop, spec) = (
                self.slot.clone(),
                stop.clone(),
                (cfg.width, cfg.height, cfg.fps),
            );
            threads.push(thread("p2c-stream-video", move || {
                write_video(stdin, slot, stop, spec, t0, skip)
            }));
        }
        {
            let (mixer, stop, skip) = (self.mixer.clone(), stop.clone(), audio_skip.clone());
            threads.push(thread("p2c-stream-audio", move || {
                write_audio(listener, mixer, stop, t0, skip)
            }));
        }
        {
            let (state, stop, key) = (self.state.clone(), stop.clone(), cfg.key.clone());
            threads.push(thread("p2c-stream-log", move || {
                watch_stderr(stderr, state, stop, key)
            }));
        }
        *self.running.lock() = Some(Running {
            child,
            stop,
            threads,
        });
        Ok(())
    }

    pub fn stop(&self) {
        let Some(r) = self.running.lock().take() else {
            return;
        };
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
        *self.state.lock() = StreamState::Idle;
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
    slot: FrameSlot,
    stop: Arc<AtomicBool>,
    (w, h, fps): (u32, u32, u32),
    t0: Instant,
    audio_skip: Arc<AtomicU64>,
) {
    let fps = fps.max(1);
    let period = Duration::from_secs_f64(1.0 / fps as f64);
    let black = Frame::new_black(w, h);
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
                .filter(|f| f.width == w && f.height == h)
                .unwrap_or(&black);
            if stdin.write_all(&f.data).is_err() {
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
fn watch_stderr(
    stderr: std::process::ChildStderr,
    state: Arc<Mutex<StreamState>>,
    stop: Arc<AtomicBool>,
    key: String,
) {
    let mut reader = BufReader::new(stderr);
    let mut last = String::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // Stats lines end in '\r', log lines in '\n'.
        let n = read_until_any(&mut reader, &mut buf);
        if n == 0 {
            break;
        }
        let line = redact(String::from_utf8_lossy(&buf).trim(), &key);
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
            last = line;
        }
    }
    if !stop.load(Ordering::SeqCst) {
        let why = if last.is_empty() {
            "ffmpeg exited".to_string()
        } else {
            last
        };
        tracing::warn!("stream ended: {why}");
        *state.lock() = StreamState::Error(why);
    }
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
            write_video(sink, FrameSlot::new(), stop2, (4, 4, 30), t0, skip)
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
        let slot = FrameSlot::new();
        let stop = Arc::new(AtomicBool::new(false));
        let feeder = {
            let (slot, stop) = (slot.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut v = 0u8;
                while !stop.load(Ordering::SeqCst) {
                    v = v.wrapping_add(1);
                    slot.put(Frame::from_bgra(4, 4, vec![v; 64]).unwrap());
                    std::thread::sleep(Duration::from_millis(33));
                }
            })
        };
        let skip = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (stop2, skip2) = (stop.clone(), skip.clone());
        let t0 = Instant::now();
        let h = std::thread::spawn(move || write_video(sink, slot, stop2, (4, 4, 30), t0, skip2));
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
            url: "rtmp://127.0.0.1:1/live".into(),
            key: "k".into(),
            width: 64,
            height: 36,
            fps: 30,
            video_kbps: 500,
        };
        let err = c
            .start(cfg, std::path::Path::new("/nonexistent/ffmpeg"))
            .unwrap_err();
        assert!(format!("{err:#}").contains("ffmpeg"), "{err:#}");
        assert!(matches!(c.state(), StreamState::Error(_)));
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
