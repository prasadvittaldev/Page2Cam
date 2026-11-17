use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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

        let mut threads = Vec::new();
        {
            let (slot, stop, spec) = (
                self.slot.clone(),
                stop.clone(),
                (cfg.width, cfg.height, cfg.fps),
            );
            threads.push(thread("p2c-stream-video", move || {
                write_video(stdin, slot, stop, spec)
            }));
        }
        {
            let (mixer, stop) = (self.mixer.clone(), stop.clone());
            threads.push(thread("p2c-stream-audio", move || {
                write_audio(listener, mixer, stop)
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

/// Write exactly `fps` frames per second (repeating the latest one when the
/// compositor is slower), so video timing stays locked to audio.
fn write_video(
    mut stdin: std::process::ChildStdin,
    slot: FrameSlot,
    stop: Arc<AtomicBool>,
    (w, h, fps): (u32, u32, u32),
) {
    let period = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let black = Frame::new_black(w, h);
    let mut start = Instant::now();
    let mut n: u32 = 0;
    while !stop.load(Ordering::Relaxed) {
        let latest = slot.latest();
        let f = latest
            .as_deref()
            .filter(|f| f.width == w && f.height == h)
            .unwrap_or(&black);
        if stdin.write_all(&f.data).is_err() {
            break;
        }
        n += 1;
        let deadline = start + period * n;
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        } else if now - deadline > Duration::from_secs(1) {
            // Fell far behind (slow encoder): restart the clock rather than burst.
            start = now;
            n = 0;
        }
    }
}

/// Accept ffmpeg's audio connection, then send 10 ms of mixed audio per tick.
fn write_audio(listener: TcpListener, mixer: Mixer, stop: Arc<AtomicBool>) {
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
    let start = Instant::now();
    let mut n: u32 = 0;
    let mut bytes = Vec::with_capacity(FRAMES * 8);
    while !stop.load(Ordering::Relaxed) {
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
