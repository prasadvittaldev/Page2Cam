//! The fan-out thread (one encoder's MPEG-TS to every sender) and the sender
//! supervisor (one copy-only ffmpeg per site, restarted on its own).
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::controller::watch_stderr;
use crate::ts::{Psi, TS};
use crate::{classify_exit, retry_delay, sender_args, SenderExit, Site, SiteState};

/// Chunks queued per sender. A sender whose queue is full is restarted.
pub const SENDER_QUEUE: usize = 64;

/// Bytes read from the encoder at a time (a multiple of `TS`, about 64 KiB).
const READ_SIZE: usize = TS * 348;

pub(crate) type Chunk = Arc<[u8]>;

/// One sender's queue, as the fan-out sees it.
pub(crate) struct Tap {
    pub id: u64,
    pub tx: SyncSender<Chunk>,
    /// Data has started (from a random-access packet on).
    pub synced: bool,
    /// Set when the queue was full and the sender is being restarted.
    pub overflow: Arc<AtomicBool>,
    /// Ends the sender (called once, on overflow).
    pub on_overflow: Box<dyn Fn() + Send>,
}

pub(crate) type Taps = Arc<Mutex<Vec<Tap>>>;

/// Copies whole TS packets to every tap; a new tap starts at the next video
/// random-access packet, preceded by the latest PAT and PMT.
pub(crate) struct Fanout {
    taps: Taps,
    psi: Psi,
}

impl Fanout {
    pub(crate) fn new(taps: Taps) -> Self {
        Fanout {
            taps,
            psi: Psi::default(),
        }
    }

    /// Hand `data` (whole packets) to every tap. Never blocks.
    pub(crate) fn feed(&mut self, data: &[u8]) {
        // Where a new sender may start, with the tables known at that point.
        let mut start: Option<(usize, Vec<u8>)> = None;
        for (i, p) in data.as_chunks::<TS>().0.iter().enumerate() {
            self.psi.see(p);
            if start.is_none() && self.psi.starts_here(p) {
                start = Some((i * TS, self.psi.headers()));
            }
        }
        let mut taps = self.taps.lock();
        if taps.is_empty() {
            return;
        }
        let whole: Chunk = Arc::from(data);
        taps.retain_mut(|t| {
            let chunk = if t.synced {
                whole.clone()
            } else if let Some((at, headers)) = &start {
                t.synced = true;
                let mut v = Vec::with_capacity(headers.len() + data.len() - at);
                v.extend_from_slice(headers);
                v.extend_from_slice(&data[*at..]);
                Arc::from(v)
            } else {
                return true;
            };
            match t.tx.try_send(chunk) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    t.overflow.store(true, Ordering::SeqCst);
                    (t.on_overflow)();
                    false
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
    }
}

/// Longest a read piece waits to be fanned out. A pipe read returns what is
/// there (often one packet), so pieces are gathered into chunks of up to
/// `READ_SIZE` or this long: `SENDER_QUEUE` chunks then cover seconds (a new
/// sender ffmpeg can take a second to start reading), not a few hundred
/// tiny pieces.
const GATHER: Duration = Duration::from_millis(100);

/// Read the encoder's stdout until it ends, keeping 188-byte alignment
/// across reads, and fan it out.
pub(crate) fn pump(mut r: impl Read, taps: Taps) {
    let mut f = Fanout::new(taps);
    let mut buf = vec![0u8; READ_SIZE];
    let mut filled = 0;
    let mut since = Instant::now();
    let flush = |f: &mut Fanout, buf: &mut [u8], filled: &mut usize| {
        let whole = *filled / TS * TS;
        if whole > 0 {
            f.feed(&buf[..whole]);
            buf.copy_within(whole..*filled, 0);
            *filled -= whole;
        }
    };
    loop {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                if buf.len() - filled < TS || since.elapsed() >= GATHER {
                    flush(&mut f, &mut buf, &mut filled);
                    since = Instant::now();
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    flush(&mut f, &mut buf, &mut filled);
}

/// The current sender process of a site, so a stop can end it.
pub(crate) type ChildSlot = Arc<Mutex<Option<Arc<Mutex<Child>>>>>;

/// Everything one site's supervisor needs.
pub(crate) struct SenderCtx {
    pub site: Site,
    /// Publish URL with the key: never logged unredacted.
    pub target: String,
    /// Every configured key, for redaction.
    pub keys: Vec<String>,
    pub ffmpeg: PathBuf,
    pub taps: Taps,
    pub next_tap: Arc<AtomicU64>,
    /// The whole stream stops.
    pub stop_all: Arc<AtomicBool>,
    /// This site stops.
    pub stop: Arc<AtomicBool>,
    pub child: ChildSlot,
    pub state: Arc<Mutex<SiteState>>,
    /// Called after the site went to `Error`.
    pub on_error: Box<dyn Fn() + Send>,
}

impl SenderCtx {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst) || self.stop_all.load(Ordering::SeqCst)
    }

    fn redact(&self, s: &str) -> String {
        let keys: Vec<&str> = self.keys.iter().map(String::as_str).collect();
        crate::redact_all(s, &keys)
    }
}

/// Run one site's sender until the site or the stream stops: restart it
/// after `retry_delay` when it exits, stop for good on an auth error.
pub(crate) fn supervise(ctx: SenderCtx) {
    let label = ctx.site.label();
    let mut attempt = 0u32;
    while !ctx.stopped() {
        let Some(exit) = run_sender(&ctx, &mut attempt) else {
            break;
        };
        if ctx.stopped() {
            break;
        }
        match exit {
            SenderExit::Auth(why) => {
                tracing::warn!("{label} refused the stream: {why}");
                *ctx.state.lock() = SiteState::Error(why);
                (ctx.on_error)();
                break;
            }
            SenderExit::Other(why) => {
                attempt += 1;
                let wait = retry_delay(attempt);
                tracing::warn!(
                    "{label} sender ended: {why}; retry {attempt} in {} s",
                    wait.as_secs()
                );
                *ctx.state.lock() = SiteState::Reconnecting { attempt };
                let until = Instant::now() + wait;
                while !ctx.stopped() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

fn spawn_sender(ctx: &SenderCtx) -> std::io::Result<Child> {
    let args = sender_args(&ctx.target);
    tracing::info!(
        "starting the {} sender: ffmpeg {}",
        ctx.site.label(),
        ctx.redact(&args.join(" "))
    );
    let mut cmd = Command::new(&ctx.ffmpeg);
    cmd.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.spawn()
}

/// One sender process from start to exit. `None` when the site or stream
/// was stopped meanwhile. Going live resets `attempt`.
fn run_sender(ctx: &SenderCtx, attempt: &mut u32) -> Option<SenderExit> {
    let mut child = match spawn_sender(ctx) {
        Ok(c) => c,
        Err(e) => return Some(SenderExit::Other(format!("could not start ffmpeg: {e}"))),
    };
    let (Some(stdin), Some(stderr)) = (child.stdin.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Some(SenderExit::Other("ffmpeg pipes missing".into()));
    };
    let child = Arc::new(Mutex::new(child));
    {
        // Checked under the slot lock: a stop either sees this child or we
        // see the stop.
        let mut slot = ctx.child.lock();
        if ctx.stopped() {
            let mut c = child.lock();
            let _ = c.kill();
            let _ = c.wait();
            return None;
        }
        *slot = Some(child.clone());
    }
    let overflow = Arc::new(AtomicBool::new(false));
    let (tx, rx) = sync_channel::<Chunk>(SENDER_QUEUE);
    let writer = std::thread::Builder::new()
        .name(format!("p2c-stream-{}-in", ctx.site.as_str()))
        .spawn(move || {
            let mut stdin = stdin;
            for chunk in rx {
                if stdin.write_all(&chunk).is_err() {
                    break;
                }
            }
        });
    let id = ctx.next_tap.fetch_add(1, Ordering::SeqCst);
    let kill = child.clone();
    ctx.taps.lock().push(Tap {
        id,
        tx,
        synced: false,
        overflow: overflow.clone(),
        on_overflow: Box::new(move || {
            let _ = kill.lock().kill();
        }),
    });
    let keys: Vec<&str> = ctx.keys.iter().map(String::as_str).collect();
    let mut live = false;
    let lines = watch_stderr(stderr, &keys, || {
        if !live {
            live = true;
            *attempt = 0;
            tracing::info!("{} is live", ctx.site.label());
            *ctx.state.lock() = SiteState::Live;
        }
    });
    // Unhook first (ends the writer), then reap the process.
    ctx.taps.lock().retain(|t| t.id != id);
    let _ = child.lock().wait();
    *ctx.child.lock() = None;
    if let Ok(w) = writer {
        let _ = w.join();
    }
    if ctx.stopped() {
        return None;
    }
    Some(if overflow.load(Ordering::SeqCst) {
        SenderExit::Other("the connection could not keep up with the stream".into())
    } else {
        classify_exit(&lines)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts::tests::{packet, pat, pmt};
    use std::sync::mpsc::Receiver;

    fn tap(taps: &Taps, id: u64) -> (Receiver<Chunk>, Arc<AtomicBool>, Arc<AtomicBool>) {
        let (tx, rx) = sync_channel(SENDER_QUEUE);
        let overflow = Arc::new(AtomicBool::new(false));
        let killed = Arc::new(AtomicBool::new(false));
        let k = killed.clone();
        taps.lock().push(Tap {
            id,
            tx,
            synced: false,
            overflow: overflow.clone(),
            on_overflow: Box::new(move || k.store(true, Ordering::SeqCst)),
        });
        (rx, overflow, killed)
    }

    #[test]
    fn new_sender_starts_at_a_video_keyframe_after_pat_and_pmt() {
        let taps: Taps = Arc::default();
        let mut f = Fanout::new(taps.clone());
        let (rx, _, _) = tap(&taps, 1);
        let audio_key = packet(0x101, true, true, &[1]);
        let video = packet(0x100, true, false, &[2]);
        let video_key = packet(0x100, true, true, &[3]);
        // Tables and an audio "keyframe", no video keyframe: nothing yet.
        f.feed(&[pat(), pmt(), audio_key, video].concat());
        assert!(rx.try_recv().is_err());
        // The keyframe comes mid-chunk: tables, then from the keyframe on.
        f.feed(&[video, video_key, audio_key].concat());
        let got = rx.try_recv().unwrap();
        assert_eq!(&got[..], &[pat(), pmt(), video_key, audio_key].concat()[..]);
        // From then on every chunk, whole.
        f.feed(&[video, audio_key].concat());
        assert_eq!(
            &rx.try_recv().unwrap()[..],
            &[video, audio_key].concat()[..]
        );
    }

    #[test]
    fn a_full_queue_restarts_only_that_sender() {
        let taps: Taps = Arc::default();
        let mut f = Fanout::new(taps.clone());
        let (slow, overflow, killed) = tap(&taps, 1);
        let (fast, ok, _) = tap(&taps, 2);
        let chunk = [pat(), pmt(), packet(0x100, true, true, &[])].concat();
        for _ in 0..SENDER_QUEUE {
            f.feed(&chunk);
            fast.try_recv().unwrap();
        }
        assert!(!overflow.load(Ordering::SeqCst));
        f.feed(&chunk);
        assert!(overflow.load(Ordering::SeqCst) && killed.load(Ordering::SeqCst));
        assert!(!ok.load(Ordering::SeqCst));
        assert_eq!(taps.lock().len(), 1, "the slow tap is unhooked");
        assert!(fast.try_recv().is_ok(), "the other sender still gets data");
        drop(slow);
    }

    /// Hands out its data `step` bytes at a time.
    struct Dribble {
        data: Vec<u8>,
        at: usize,
        step: usize,
    }
    impl Read for Dribble {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = self.step.min(out.len()).min(self.data.len() - self.at);
            out[..n].copy_from_slice(&self.data[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
    }

    #[test]
    fn pump_keeps_packet_alignment_across_reads() {
        let taps: Taps = Arc::default();
        let (rx, _, _) = tap(&taps, 1);
        let mut data = [pat(), pmt(), packet(0x100, true, true, &[9])].concat();
        for i in 0..40u8 {
            data.extend_from_slice(&packet(0x100, i == 0, false, &[i]));
        }
        let expect = data.clone();
        pump(
            Dribble {
                data,
                at: 0,
                step: 100,
            },
            taps,
        );
        let (mut got, mut chunks) = (Vec::new(), 0);
        while let Ok(c) = rx.try_recv() {
            assert_eq!(c.len() % TS, 0);
            got.extend_from_slice(&c);
            chunks += 1;
        }
        // 81 reads of 100 bytes arriving at once are gathered, not sent one by one.
        assert_eq!(chunks, 1);
        // Tables cached, then everything from the keyframe on.
        let mut want = [pat(), pmt()].concat();
        want.extend_from_slice(&expect[2 * TS..]);
        assert_eq!(got, want);
    }
}
