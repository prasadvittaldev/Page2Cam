//! Streams 4 s to a local ffmpeg RTMP listener and checks the recording.
//! Needs ffmpeg + ffprobe on PATH: cargo test -p p2c-stream --test rtmp_local -- --ignored
use std::process::{Command, Stdio};
use std::time::Duration;

use p2c_audio::{MixSettings, Mixer};
use p2c_compositor::{FrameSink, Output};
use p2c_core::{AudioRing, Frame};
use p2c_stream::{find_ffmpeg, StreamConfig, StreamController, StreamState};

#[test]
#[ignore]
fn streams_h264_and_aac_to_rtmp() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("debug")
        .try_init();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("got.flv");
    let mut server = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-listen",
            "1",
            "-i",
            "rtmp://127.0.0.1:19350/live/testkey",
            "-c",
            "copy",
        ])
        .arg(&out)
        .stdin(Stdio::null())
        .spawn()
        .expect("ffmpeg listener");
    std::thread::sleep(Duration::from_millis(800));

    let page = AudioRing::new(2000);
    let mixer = Mixer::new(page.clone(), AudioRing::new(100), MixSettings::default());
    let c = StreamController::new(mixer);
    let mut sink = c.sink();
    let cfg = StreamConfig {
        destinations: vec![p2c_stream::Destination {
            site: p2c_stream::Site::YouTube,
            url: "rtmp://127.0.0.1:19350/live".into(),
            key: "testkey".into(),
        }],
        width: 320,
        height: 180,
        fps: 30,
        video_kbps: 800,
        pix_fmt: "bgra",
        encoder: p2c_stream::Encoder::X264,
    };
    c.start(cfg, &find_ffmpeg().expect("ffmpeg on PATH"))
        .unwrap();
    let mut live = false;
    for i in 0..120 {
        sink.push(&Output {
            bgra: Some(std::sync::Arc::new(Frame::new_black(320, 180))),
            ..Output::default()
        })
        .unwrap();
        let tone: Vec<f32> = (0..1600)
            .map(|j| (((i * 1600 + j) / 2) as f32 * 0.06).sin() * 0.3)
            .collect();
        page.push(&tone);
        live |= c.state() == StreamState::Live;
        std::thread::sleep(Duration::from_millis(33));
    }
    assert!(live, "never went live; state = {}", c.state());
    c.stop();
    assert_eq!(c.state(), StreamState::Idle);
    let _ = server.wait_timeout_secs(10);

    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ])
        .arg(&out)
        .output()
        .unwrap();
    let codecs = String::from_utf8_lossy(&probe.stdout);
    assert!(
        codecs.contains("h264") && codecs.contains("aac"),
        "codecs: {codecs:?}"
    );
}

trait WaitTimeout {
    fn wait_timeout_secs(&mut self, s: u64) -> Option<std::process::ExitStatus>;
}
impl WaitTimeout for std::process::Child {
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
