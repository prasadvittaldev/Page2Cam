//! Streams to local ffmpeg RTMP listeners and checks the recordings and the
//! per-site states. Needs ffmpeg + ffprobe (next to each other, on PATH):
//! cargo test -p p2c-stream --test rtmp_local -- --ignored
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use p2c_audio::{MixSettings, Mixer};
use p2c_compositor::{FrameSink, Output};
use p2c_core::{AudioRing, Frame};
use p2c_stream::{
    find_ffmpeg, retry_delay, Destination, Site, SiteState, StreamConfig, StreamController,
    StreamState,
};

fn ffmpeg_or_skip() -> Option<PathBuf> {
    let ff = find_ffmpeg();
    if ff.is_none() {
        eprintln!("SKIPPED: no ffmpeg next to the test or on PATH");
    }
    ff
}

fn ffprobe(ff: &Path) -> PathBuf {
    ff.with_file_name(if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    })
}

/// A local RTMP server recording `rtmp://127.0.0.1:<port>/live/<key>` to `out`.
fn listen(ff: &Path, port: u16, key: &str, out: &Path) -> Child {
    let c = Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-listen", "1"])
        .args(["-f", "flv", "-i"])
        .arg(format!("rtmp://127.0.0.1:{port}/live/{key}"))
        .args(["-c", "copy"])
        .arg(out)
        .stdin(Stdio::null())
        .spawn()
        .expect("ffmpeg listener");
    std::thread::sleep(Duration::from_millis(800));
    c
}

fn dest(site: Site, port: u16, key: &str) -> Destination {
    Destination {
        site,
        url: format!("rtmp://127.0.0.1:{port}/live"),
        key: key.into(),
    }
}

fn cfg(destinations: Vec<Destination>) -> StreamConfig {
    StreamConfig {
        destinations,
        width: 320,
        height: 180,
        fps: 30,
        video_kbps: 800,
        pix_fmt: "bgra",
        encoder: p2c_stream::Encoder::X264,
    }
}

/// A controller fed with black frames and a tone at 30 fps until dropped.
struct Rig {
    c: StreamController,
    stop: Arc<AtomicBool>,
    feeder: Option<JoinHandle<()>>,
}

impl Rig {
    fn new() -> Rig {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .try_init();
        let page = AudioRing::new(2000);
        let mixer = Mixer::new(page.clone(), AudioRing::new(100), MixSettings::default());
        let c = StreamController::new(mixer);
        let mut sink = c.sink();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let feeder = std::thread::spawn(move || {
            let mut i = 0usize;
            while !s.load(Ordering::SeqCst) {
                sink.push(&Output {
                    bgra: Some(Arc::new(Frame::new_black(320, 180))),
                    ..Output::default()
                })
                .unwrap();
                let tone: Vec<f32> = (0..1600)
                    .map(|j| (((i * 1600 + j) / 2) as f32 * 0.06).sin() * 0.3)
                    .collect();
                page.push(&tone);
                i += 1;
                std::thread::sleep(Duration::from_millis(33));
            }
        });
        Rig {
            c,
            stop,
            feeder: Some(feeder),
        }
    }

    fn site(&self, site: Site) -> SiteState {
        self.c
            .sites()
            .into_iter()
            .find(|(s, _)| *s == site)
            .map(|(_, st)| st)
            .expect("every site is listed")
    }

    /// Wait up to `secs` for `f`; panics with the states otherwise.
    fn wait(&self, secs: u64, what: &str, f: impl Fn(&Rig) -> bool) {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if f(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "{what}: not within {secs} s; state {}, sites {:?}",
            self.c.state(),
            self.c.sites()
        );
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(f) = self.feeder.take() {
            let _ = f.join();
        }
        self.c.stop();
    }
}

fn probe_codecs(ff: &Path, file: &Path) -> String {
    let out = Command::new(ffprobe(ff))
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .expect("ffprobe");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn assert_h264_aac(ff: &Path, file: &Path) {
    let codecs = probe_codecs(ff, file);
    assert!(
        codecs.contains("h264") && codecs.contains("aac"),
        "{}: codecs {codecs:?}",
        file.display()
    );
}

/// The recording's duration in seconds (0 if unreadable).
fn duration(ff: &Path, file: &Path) -> f64 {
    let out = Command::new(ffprobe(ff))
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .expect("ffprobe");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0.0)
}

#[test]
#[ignore]
fn streams_h264_and_aac_to_rtmp() {
    let Some(ff) = ffmpeg_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("got.flv");
    let mut server = listen(&ff, 19350, "testkey", &out);
    let rig = Rig::new();
    rig.c
        .start(cfg(vec![dest(Site::YouTube, 19350, "testkey")]), &ff)
        .unwrap();
    rig.wait(20, "live", |r| r.c.state() == StreamState::Live);
    std::thread::sleep(Duration::from_secs(4));
    rig.c.stop();
    assert_eq!(rig.c.state(), StreamState::Idle);
    let _ = server.wait_timeout_secs(10);
    assert_h264_aac(&ff, &out);
}

#[test]
#[ignore]
fn both_sites_receive_one_encode() {
    let Some(ff) = ffmpeg_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (fa, fb) = (dir.path().join("a.flv"), dir.path().join("b.flv"));
    let mut la = listen(&ff, 19351, "key-a-123", &fa);
    let mut lb = listen(&ff, 19352, "key-b-456", &fb);
    let rig = Rig::new();
    rig.c
        .start(
            cfg(vec![
                dest(Site::YouTube, 19351, "key-a-123"),
                dest(Site::Twitch, 19352, "key-b-456"),
            ]),
            &ff,
        )
        .unwrap();
    rig.wait(20, "both live", |r| {
        r.site(Site::YouTube) == SiteState::Live && r.site(Site::Twitch) == SiteState::Live
    });
    assert_eq!(rig.c.state(), StreamState::Live);
    assert_eq!(rig.c.encoder_count(), 1, "one encode for both sites");
    assert_eq!(rig.c.sender_count(), 2);
    std::thread::sleep(Duration::from_secs(6));
    rig.c.stop();
    let _ = la.wait_timeout_secs(10);
    let _ = lb.wait_timeout_secs(10);
    assert_h264_aac(&ff, &fa);
    assert_h264_aac(&ff, &fb);
}

#[test]
#[ignore]
fn one_site_dropping_leaves_the_other_live() {
    let Some(ff) = ffmpeg_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (fa, fb) = (dir.path().join("a.flv"), dir.path().join("b.flv"));
    let mut la = listen(&ff, 19353, "key-a-123", &fa);
    let mut lb = listen(&ff, 19354, "key-b-456", &fb);
    let rig = Rig::new();
    rig.c
        .start(
            cfg(vec![
                dest(Site::YouTube, 19353, "key-a-123"),
                dest(Site::Twitch, 19354, "key-b-456"),
            ]),
            &ff,
        )
        .unwrap();
    rig.wait(20, "both live", |r| {
        r.site(Site::YouTube) == SiteState::Live && r.site(Site::Twitch) == SiteState::Live
    });
    let t_live = Instant::now();
    std::thread::sleep(Duration::from_secs(3));
    let _ = lb.kill();
    let _ = lb.wait();
    rig.wait(15, "B reconnecting", |r| {
        matches!(r.site(Site::Twitch), SiteState::Reconnecting { .. })
    });
    assert_eq!(rig.site(Site::YouTube), SiteState::Live);
    assert_eq!(rig.c.state(), StreamState::Live);
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(rig.site(Site::YouTube), SiteState::Live);
    // B comes back: within the current retry delay (+ one slipped retry) + 5 s.
    let attempt = match rig.site(Site::Twitch) {
        SiteState::Reconnecting { attempt } => attempt,
        s => panic!("B is {s}"),
    };
    let fb2 = dir.path().join("b2.flv");
    let mut lb2 = listen(&ff, 19354, "key-b-456", &fb2);
    let secs = (retry_delay(attempt + 1) + Duration::from_secs(5)).as_secs();
    rig.wait(secs, "B live again", |r| {
        r.site(Site::Twitch) == SiteState::Live
    });
    assert_eq!(rig.site(Site::YouTube), SiteState::Live);
    let ran = t_live.elapsed().as_secs_f64();
    rig.c.stop();
    let _ = la.wait_timeout_secs(10);
    let _ = lb2.wait_timeout_secs(10);
    // A kept receiving through B's outage: its recording covers the whole
    // run (file sizes can't be read while ffmpeg writes on Windows).
    let got = duration(&ff, &fa);
    assert!(
        got >= ran - 1.5,
        "A recorded {got:.1} s of a {ran:.1} s run"
    );
    assert_h264_aac(&ff, &fb2);
}

#[test]
#[ignore]
fn untick_and_tick_while_live() {
    let Some(ff) = ffmpeg_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (fa, fb) = (dir.path().join("a.flv"), dir.path().join("b.flv"));
    let mut la = listen(&ff, 19355, "key-a-123", &fa);
    let mut lb = listen(&ff, 19356, "key-b-456", &fb);
    let a = dest(Site::YouTube, 19355, "key-a-123");
    let b = dest(Site::Twitch, 19356, "key-b-456");
    let rig = Rig::new();
    rig.c.start(cfg(vec![a.clone(), b.clone()]), &ff).unwrap();
    rig.wait(20, "both live", |r| {
        r.site(Site::YouTube) == SiteState::Live && r.site(Site::Twitch) == SiteState::Live
    });
    rig.c.set_destinations(vec![a.clone()]).unwrap();
    assert_eq!(rig.site(Site::Twitch), SiteState::Idle);
    assert_eq!(rig.c.sender_count(), 1, "B's sender is gone");
    assert_eq!(rig.site(Site::YouTube), SiteState::Live);
    let _ = lb.wait_timeout_secs(10);
    let fb2 = dir.path().join("b2.flv");
    let mut lb2 = listen(&ff, 19356, "key-b-456", &fb2);
    rig.c.set_destinations(vec![a, b]).unwrap();
    // Straight to live: a sender that starts while the stream runs must not
    // be restarted (e.g. its queue filling while ffmpeg starts up).
    let hiccup = std::cell::Cell::new(false);
    rig.wait(20, "B live again", |r| {
        let s = r.site(Site::Twitch);
        hiccup.set(hiccup.get() || !matches!(s, SiteState::Connecting | SiteState::Live));
        s == SiteState::Live
    });
    assert!(!hiccup.get(), "B was restarted on the way to live");
    assert_eq!(rig.c.encoder_count(), 1);
    assert_eq!(rig.c.sender_count(), 2);
    rig.c.stop();
    let _ = la.wait_timeout_secs(10);
    let _ = lb2.wait_timeout_secs(10);
    assert_h264_aac(&ff, &fb2);
}

#[test]
#[ignore]
fn encoder_death_stops_everything() {
    let Some(ff) = ffmpeg_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (fa, fb) = (dir.path().join("a.flv"), dir.path().join("b.flv"));
    let mut la = listen(&ff, 19357, "key-a-123", &fa);
    let mut lb = listen(&ff, 19358, "key-b-456", &fb);
    let rig = Rig::new();
    rig.c
        .start(
            cfg(vec![
                dest(Site::YouTube, 19357, "key-a-123"),
                dest(Site::Twitch, 19358, "key-b-456"),
            ]),
            &ff,
        )
        .unwrap();
    rig.wait(20, "both live", |r| {
        r.c.sender_count() == 2 && r.c.state() == StreamState::Live
    });
    rig.c.kill_encoder();
    rig.wait(3, "error", |r| matches!(r.c.state(), StreamState::Error(_)));
    rig.wait(3, "senders gone", |r| r.c.sender_count() == 0);
    assert_eq!(rig.c.encoder_count(), 0);
    let _ = la.wait_timeout_secs(10);
    let _ = lb.wait_timeout_secs(10);
}

#[test]
#[ignore]
fn stop_during_backoff_starts_nothing() {
    let Some(ff) = ffmpeg_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let fa = dir.path().join("a.flv");
    let mut la = listen(&ff, 19359, "key-a-123", &fa);
    let rig = Rig::new();
    rig.c
        .start(
            cfg(vec![
                dest(Site::YouTube, 19359, "key-a-123"),
                // Nothing listens on port 1.
                dest(Site::Twitch, 1, "key-b-456"),
            ]),
            &ff,
        )
        .unwrap();
    rig.wait(20, "B in backoff", |r| {
        r.site(Site::Twitch) == SiteState::Reconnecting { attempt: 1 }
    });
    rig.c.stop();
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(rig.c.sender_count(), 0);
    assert!(
        rig.c.sites().iter().all(|(_, s)| *s == SiteState::Idle),
        "{:?}",
        rig.c.sites()
    );
    assert_eq!(rig.c.state(), StreamState::Idle);
    let _ = la.wait_timeout_secs(10);
}

/// A destination the sender classifies as refused (`Auth`): nothing listens,
/// and ffmpeg's error line names the URL, which says "unauthorized".
fn refused(site: Site) -> Destination {
    Destination {
        site,
        url: "rtmp://127.0.0.1:1/unauthorized".into(),
        key: format!("key-{}", site.as_str()),
    }
}

#[test]
#[ignore]
fn every_site_failing_stops_the_stream() {
    let Some(ff) = ffmpeg_or_skip() else { return };
    let rig = Rig::new();
    rig.c
        .start(
            cfg(vec![refused(Site::Twitch), refused(Site::YouTube)]),
            &ff,
        )
        .unwrap();
    rig.wait(20, "stopped by itself", |r| {
        matches!(r.c.state(), StreamState::Error(_)) && r.c.encoder_count() == 0
    });
    let StreamState::Error(e) = rig.c.state() else {
        unreachable!()
    };
    assert!(
        e.starts_with("YouTube: ") && e.contains("; Twitch: "),
        "{e}"
    );
    assert!(
        !e.contains("key-youtube") && !e.contains("key-twitch"),
        "{e}"
    );
    assert_eq!(rig.c.sender_count(), 0);
    assert!(
        rig.c
            .sites()
            .iter()
            .all(|(_, s)| matches!(s, SiteState::Error(_))),
        "{:?}",
        rig.c.sites()
    );
}

#[test]
#[ignore]
fn unticking_down_to_failed_sites_stops_the_stream() {
    let Some(ff) = ffmpeg_or_skip() else { return };
    let rig = Rig::new();
    let twitch = Destination {
        site: Site::Twitch,
        url: "rtmp://127.0.0.1:1/live".into(),
        key: "key-b-456".into(),
    };
    rig.c
        .start(cfg(vec![refused(Site::YouTube), twitch]), &ff)
        .unwrap();
    rig.wait(20, "YouTube refused, Twitch retrying", |r| {
        matches!(r.site(Site::YouTube), SiteState::Error(_))
            && matches!(r.site(Site::Twitch), SiteState::Reconnecting { .. })
    });
    assert_eq!(
        rig.c.state(),
        StreamState::Connecting,
        "a retrying site keeps it running"
    );
    rig.c
        .set_destinations(vec![refused(Site::YouTube)])
        .unwrap();
    let StreamState::Error(e) = rig.c.state() else {
        panic!("still {}", rig.c.state())
    };
    assert!(e.starts_with("YouTube: ") && !e.contains("Twitch"), "{e}");
    assert_eq!(rig.c.encoder_count() + rig.c.sender_count(), 0);
}

trait WaitTimeout {
    fn wait_timeout_secs(&mut self, s: u64) -> Option<std::process::ExitStatus>;
}
impl WaitTimeout for Child {
    fn wait_timeout_secs(&mut self, s: u64) -> Option<std::process::ExitStatus> {
        for _ in 0..s * 10 {
            if let Ok(Some(st)) = self.try_wait() {
                return Some(st);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.kill();
        self.wait().ok()
    }
}
