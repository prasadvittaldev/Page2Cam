use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use p2c_core::{CHANNELS, SAMPLE_RATE};

#[derive(Clone, Debug)]
pub struct StreamConfig {
    /// Where the stream goes (one sender per destination).
    pub destinations: Vec<Destination>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_kbps: u32,
    /// Raw video input format: `"bgra"` (`Output::bgra`) or `"nv12"`
    /// (`Output::nv12`, BT.709 limited range).
    pub pix_fmt: &'static str,
    /// H.264 encoder. `StreamController::start` replaces this with the
    /// controller's own (`set_encoder`).
    pub encoder: Encoder,
}

/// A streaming site.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Site {
    YouTube,
    Twitch,
}

impl Site {
    pub fn as_str(self) -> &'static str {
        match self {
            Site::YouTube => "youtube",
            Site::Twitch => "twitch",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Site::YouTube => "YouTube",
            Site::Twitch => "Twitch",
        }
    }
}

/// One RTMP destination.
#[derive(Clone)]
pub struct Destination {
    pub site: Site,
    /// RTMP server URL (e.g. rtmp://a.rtmp.youtube.com/live2).
    pub url: String,
    /// Stream key, appended as the last path segment. Empty: `url` is complete.
    pub key: String,
}

/// Never prints the key (`****` when there is one).
impl std::fmt::Debug for Destination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Destination")
            .field("site", &self.site)
            .field("url", &redact(&self.url, &self.key))
            .field("key", &if self.key.is_empty() { "" } else { "****" })
            .finish()
    }
}

impl Destination {
    /// Full publish URL. Contains the key: never log it unredacted.
    pub fn target(&self) -> String {
        if self.key.is_empty() {
            self.url.clone()
        } else {
            format!("{}/{}", self.url.trim_end_matches('/'), self.key)
        }
    }
}

/// H.264 encoder behind the RTMP stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Encoder {
    Nvenc,
    Qsv,
    Amf,
    #[default]
    X264,
}

impl Encoder {
    /// The ffmpeg `-c:v` name.
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            Encoder::Nvenc => "h264_nvenc",
            Encoder::Qsv => "h264_qsv",
            Encoder::Amf => "h264_amf",
            Encoder::X264 => "libx264",
        }
    }
}

impl std::fmt::Display for Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Encoder::Nvenc => "NVENC",
            Encoder::Qsv => "Quick Sync",
            Encoder::Amf => "AMF",
            Encoder::X264 => "x264",
        })
    }
}

/// Encoders to try for a GPU vendor (PCI vendor id), best first; x264 always last.
pub fn candidates(vendor_id: Option<u32>) -> Vec<Encoder> {
    let mut v = match vendor_id {
        Some(0x10DE) => vec![Encoder::Nvenc],
        Some(0x8086) => vec![Encoder::Qsv],
        Some(0x1002) => vec![Encoder::Amf],
        _ => vec![],
    };
    v.push(Encoder::X264);
    v
}

/// The encoder's own options (everything after `-c:v <name>`), shared by the
/// stream and the probe so a probe that passes means the stream's encoder
/// opens too.
pub fn encoder_options(e: Encoder, fps: u32, video_kbps: u32) -> Vec<String> {
    let gop = (fps * 2).to_string();
    let kbps = format!("{video_kbps}k");
    let buf = format!("{}k", video_kbps * 2);
    let mut a: Vec<String> = Vec::new();
    let rate = |a: &mut Vec<String>, with_buf: bool| {
        a.extend(["-b:v", &kbps, "-maxrate", &kbps].map(String::from));
        if with_buf {
            a.extend(["-bufsize", &buf].map(String::from));
        }
    };
    match e {
        Encoder::X264 => {
            a.extend(["-preset", "veryfast", "-bf", "2", "-pix_fmt", "yuv420p"].map(String::from));
            rate(&mut a, true);
            a.extend(["-g", &gop, "-keyint_min", &gop, "-sc_threshold", "0"].map(String::from));
        }
        Encoder::Nvenc => {
            a.extend(
                [
                    "-preset", "p5", "-tune", "hq", "-rc", "cbr", "-pix_fmt", "nv12",
                ]
                .map(String::from),
            );
            rate(&mut a, true);
            a.extend(["-g", &gop, "-bf", "2"].map(String::from));
        }
        Encoder::Qsv => {
            a.extend(["-preset", "medium", "-pix_fmt", "nv12"].map(String::from));
            rate(&mut a, false);
            a.extend(["-g", &gop, "-bf", "2"].map(String::from));
        }
        Encoder::Amf => {
            a.extend(["-quality", "balanced", "-rc", "cbr", "-pix_fmt", "nv12"].map(String::from));
            rate(&mut a, true);
            a.extend(["-g", &gop, "-bf", "2"].map(String::from));
        }
    }
    a
}

/// The probe's ffmpeg arguments: a few black 640x360 frames through `e` with
/// the stream's options, to a null output. (Smaller frames gave false
/// negatives on Quick Sync and AMF.)
pub fn probe_args(e: Encoder, fps: u32, video_kbps: u32) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"]
        .map(String::from)
        .to_vec();
    a.push(format!("color=c=black:s=640x360:r={}", fps.max(1)));
    a.extend(["-frames:v", "5", "-c:v", e.ffmpeg_name()].map(String::from));
    a.extend(encoder_options(e, fps, video_kbps));
    a.extend(["-f", "null", "-"].map(String::from));
    a
}

/// Whether `e` really encodes here, with the stream's options at `fps` and
/// `video_kbps`: runs ffmpeg on a few black frames to a null output (an
/// `-encoders` listing says nothing about the driver or the GPU). Gives up
/// after 10 s.
pub fn probe(ffmpeg: &Path, e: Encoder, fps: u32, video_kbps: u32) -> bool {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(probe_args(e, fps, video_kbps))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Encoder command line: raw BGRA or NV12 video on stdin, f32le stereo audio from a TCP
/// socket we listen on, H.264 + AAC as MPEG-TS on stdout (the senders copy it to RTMP).
pub fn encoder_args(cfg: &StreamConfig, audio_port: u16) -> Vec<String> {
    let mut a: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "info",
        "-stats_period",
        "1",
        // Raw inputs have fully specified formats: skip stream analysis, which
        // would otherwise wait seconds for real-time audio before starting.
        "-probesize",
        "32",
        "-analyzeduration",
        "0",
        "-thread_queue_size",
        "512",
        "-f",
        "rawvideo",
        "-pix_fmt",
        cfg.pix_fmt,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if cfg.pix_fmt == "nv12" {
        // Our NV12 is BT.709 limited range (p2c_core::bgra_to_nv12).
        a.extend(["-colorspace", "bt709", "-color_range", "tv"].map(String::from));
    }
    a.push("-s".into());
    a.push(format!("{}x{}", cfg.width, cfg.height));
    a.extend([
        "-framerate".to_string(),
        cfg.fps.to_string(),
        "-i".into(),
        "pipe:0".into(),
    ]);
    a.extend(
        [
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-thread_queue_size",
            "512",
            "-f",
            "f32le",
            "-ar",
        ]
        .map(String::from),
    );
    a.push(SAMPLE_RATE.to_string());
    a.push("-ac".into());
    a.push(CHANNELS.to_string());
    a.push("-i".into());
    a.push(format!("tcp://127.0.0.1:{audio_port}"));
    a.extend(
        [
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            cfg.encoder.ffmpeg_name(),
        ]
        .map(String::from),
    );
    a.extend(encoder_options(cfg.encoder, cfg.fps, cfg.video_kbps));
    if cfg.pix_fmt == "nv12" {
        // Say BT.709 limited in the H.264 VUI whatever the encoder does.
        a.extend(
            [
                "-colorspace",
                "bt709",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
                "-color_range",
                "tv",
            ]
            .map(String::from),
        );
    }
    for s in ["-c:a", "aac", "-b:a", "128k", "-ar", "48000"] {
        a.push(s.to_string());
    }
    a.extend(["-f", "mpegts", "-mpegts_flags", "+resend_headers", "pipe:1"].map(String::from));
    a
}

/// Sender command line: MPEG-TS on stdin copied unchanged into FLV for one RTMP target.
pub fn sender_args(target: &str) -> Vec<String> {
    [
        "-hide_banner",
        "-loglevel",
        "info",
        "-stats_period",
        "1",
        "-f",
        "mpegts",
        "-i",
        "pipe:0",
        "-c",
        "copy",
        "-f",
        "flv",
        target,
    ]
    .map(String::from)
    .to_vec()
}

/// ffmpeg stderr text that means the video encoder could not be opened
/// (driver, GPU or option trouble), as opposed to network/server errors.
const ENCODER_ERRORS: &[&str] = &[
    "error while opening encoder",
    "no capable devices found",
    "initializeencoder failed",
    "openencodesessionex failed",
    "error initializing an internal mfx session",
    "createcomponent",
    "cannot load",
    "not supported",
];

/// Whether a stream that just ended should be started again with x264:
/// `encoder` is a hardware encoder, ffmpeg exited before going live
/// (`exited_while_connecting`), and its last stderr lines show the encoder
/// failing to open.
pub fn retry_with_x264(encoder: Encoder, exited_while_connecting: bool, stderr: &[String]) -> bool {
    encoder != Encoder::X264
        && exited_while_connecting
        && stderr.iter().any(|l| {
            let l = l.to_ascii_lowercase();
            ENCODER_ERRORS.iter().any(|e| l.contains(e))
        })
}

/// Replace every occurrence of the stream key with `****`.
pub fn redact(s: &str, key: &str) -> String {
    if key.is_empty() {
        s.to_string()
    } else {
        s.replace(key, "****")
    }
}

/// `redact` for every non-empty key, longest first (a key inside another
/// key must not leave part of the longer one visible).
pub fn redact_all(s: &str, keys: &[&str]) -> String {
    let mut keys = keys.to_vec();
    keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
    keys.iter().fold(s.to_string(), |acc, k| redact(&acc, k))
}

/// `ffmpeg(.exe)` next to our executable, else on PATH.
pub fn find_ffmpeg() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let beside = std::env::current_exe().ok()?.parent()?.join(name);
    if beside.is_file() {
        return Some(beside);
    }
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> StreamConfig {
        cfg_with(vec![Destination {
            site: Site::YouTube,
            url: "rtmp://a.rtmp.youtube.com/live2/".into(),
            key: "abcd-1234-secret".into(),
        }])
    }

    fn cfg_with(destinations: Vec<Destination>) -> StreamConfig {
        StreamConfig {
            destinations,
            width: 1920,
            height: 1080,
            fps: 30,
            video_kbps: 4500,
            pix_fmt: "bgra",
            encoder: Encoder::X264,
        }
    }

    #[test]
    fn debug_hides_the_keys() {
        let c = cfg_with(vec![
            Destination {
                site: Site::YouTube,
                url: "rtmp://a.rtmp.youtube.com/live2".into(),
                key: "yt-secret-key".into(),
            },
            Destination {
                site: Site::Twitch,
                url: "rtmp://live.twitch.tv/app".into(),
                key: String::new(),
            },
        ]);
        let d = format!("{c:?}");
        assert!(!d.contains("yt-secret-key"), "{d}");
        assert!(
            d.contains("****") && d.contains("rtmp://live.twitch.tv/app"),
            "{d}"
        );
        assert!(!format!("{:?}", c.destinations[0]).contains("yt-secret-key"));
    }

    fn with(e: Encoder) -> String {
        encoder_args(
            &StreamConfig {
                encoder: e,
                video_kbps: 6000,
                ..cfg()
            },
            50123,
        )
        .join(" ")
    }

    #[test]
    fn nvidia_tries_nvenc_first() {
        assert_eq!(
            candidates(Some(0x10DE)),
            vec![Encoder::Nvenc, Encoder::X264]
        );
        assert_eq!(candidates(Some(0x8086)), vec![Encoder::Qsv, Encoder::X264]);
        assert_eq!(candidates(Some(0x1002)), vec![Encoder::Amf, Encoder::X264]);
    }

    #[test]
    fn unknown_vendor_is_x264_only() {
        assert_eq!(candidates(Some(0x1234)), vec![Encoder::X264]);
        assert_eq!(candidates(None), vec![Encoder::X264]);
    }

    #[test]
    fn display_names() {
        let n: Vec<_> = [Encoder::Nvenc, Encoder::Qsv, Encoder::Amf, Encoder::X264]
            .iter()
            .map(|e| e.to_string())
            .collect();
        assert_eq!(n, ["NVENC", "Quick Sync", "AMF", "x264"]);
    }

    #[test]
    fn nvenc_args_cbr_gop_bframes() {
        let s = with(Encoder::Nvenc);
        for want in [
            "-c:v h264_nvenc",
            "-preset p5",
            "-tune hq",
            "-rc cbr",
            "-b:v 6000k",
            "-maxrate 6000k",
            "-bufsize 12000k",
            "-g 60",
            "-bf 2",
        ] {
            assert!(s.contains(want), "{want} in {s}");
        }
        assert!(
            !s.contains("libx264") && !s.contains("-sc_threshold"),
            "{s}"
        );
    }

    #[test]
    fn qsv_args_cbr_gop_bframes() {
        let s = with(Encoder::Qsv);
        for want in [
            "-c:v h264_qsv",
            "-preset medium",
            "-b:v 6000k",
            "-maxrate 6000k",
            "-g 60",
            "-bf 2",
        ] {
            assert!(s.contains(want), "{want} in {s}");
        }
        assert!(!s.contains("libx264"), "{s}");
    }

    #[test]
    fn amf_args_cbr_gop_bframes() {
        let s = with(Encoder::Amf);
        for want in [
            "-c:v h264_amf",
            "-quality balanced",
            "-rc cbr",
            "-b:v 6000k",
            "-g 60",
            "-bf 2",
        ] {
            assert!(s.contains(want), "{want} in {s}");
        }
        assert!(!s.contains("libx264"), "{s}");
    }

    #[test]
    fn x264_args_unchanged() {
        let s = with(Encoder::X264);
        assert!(
            s.contains(
                "-c:v libx264 -preset veryfast -bf 2 -pix_fmt yuv420p -b:v 6000k \
                 -maxrate 6000k -bufsize 12000k -g 60 -keyint_min 60 -sc_threshold 0 -c:a aac"
            ),
            "{s}"
        );
    }

    #[test]
    fn nv12_output_is_tagged_bt709_limited() {
        for e in [Encoder::Nvenc, Encoder::Qsv, Encoder::Amf, Encoder::X264] {
            let s = encoder_args(
                &StreamConfig {
                    pix_fmt: "nv12",
                    encoder: e,
                    ..cfg()
                },
                1,
            )
            .join(" ");
            assert!(
                s.contains(
                    "-colorspace bt709 -color_primaries bt709 -color_trc bt709 \
                     -color_range tv -c:a aac"
                ),
                "{e}: {s}"
            );
        }
        assert!(!with(Encoder::Nvenc).contains("-color_trc"));
    }

    const ALL: [Encoder; 4] = [Encoder::Nvenc, Encoder::Qsv, Encoder::Amf, Encoder::X264];

    /// The `-c:v <name>` option and the `n` arguments after it.
    fn after_cv(args: &[String], n: usize) -> Vec<String> {
        let i = args.iter().position(|a| a == "-c:v").expect("-c:v");
        args[i + 1..i + 2 + n].to_vec()
    }

    #[test]
    fn probe_uses_the_streams_encoder_options() {
        for e in ALL {
            let opts = encoder_options(e, 30, 6000);
            let mut want = vec![e.ffmpeg_name().to_string()];
            want.extend(opts.iter().cloned());
            let stream = encoder_args(
                &StreamConfig {
                    encoder: e,
                    video_kbps: 6000,
                    ..cfg()
                },
                1,
            );
            assert_eq!(after_cv(&stream, opts.len()), want, "{e}: stream");
            // Nothing encoder-specific outside that block.
            let i = stream.iter().position(|a| a == "-c:v").unwrap() + 2 + opts.len();
            assert_eq!(stream[i], "-c:a", "{e}");
            let probe = probe_args(e, 30, 6000);
            assert_eq!(after_cv(&probe, opts.len()), want, "{e}: probe");
            let p = probe.join(" ");
            assert!(p.contains("s=640x360"), "{p}");
            assert!(p.ends_with("-f null -"), "{p}");
        }
    }

    #[test]
    fn x264_retry_only_for_a_hardware_encoder_failing_to_open() {
        let lines = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let nvenc_fail = lines(&[
            "[h264_nvenc @ 000001] OpenEncodeSessionEx failed: unsupported device (2)",
            "[h264_nvenc @ 000001] No capable devices found",
            "Error while opening encoder for output stream #0:0 - maybe incorrect parameters",
        ]);
        let qsv_fail = lines(&[
            "[h264_qsv @ 0001] Error initializing an internal MFX session: unsupported (-3)",
        ]);
        let amf_fail = lines(&[
            "[h264_amf @ 0001] CreateComponent(AMFVideoEncoderVCE_AVC) failed with error 30",
        ]);
        let amf_load = lines(&["[h264_amf @ 0001] Cannot load amfrt64.dll"]);
        let nv_init = lines(&["[h264_nvenc @ 0001] InitializeEncoder failed: invalid param (8)"]);
        let bframes = lines(&["[h264_nvenc @ 0001] B frames as references are not supported"]);
        let network = lines(&[
            "[rtmp @ 0001] Cannot open connection tcp://a.rtmp.youtube.com:1935",
            "Error opening output rtmp://a.rtmp.youtube.com/live2/****: I/O error",
        ]);
        assert!(retry_with_x264(Encoder::Nvenc, true, &nvenc_fail));
        assert!(retry_with_x264(Encoder::Qsv, true, &qsv_fail));
        assert!(retry_with_x264(Encoder::Amf, true, &amf_fail));
        assert!(retry_with_x264(Encoder::Amf, true, &amf_load));
        assert!(retry_with_x264(Encoder::Nvenc, true, &nv_init));
        assert!(retry_with_x264(Encoder::Nvenc, true, &bframes));
        // x264 itself failing: nothing to fall back to.
        assert!(!retry_with_x264(Encoder::X264, true, &nvenc_fail));
        // Was live already: an encoder that opened is not the problem.
        assert!(!retry_with_x264(Encoder::Nvenc, false, &nvenc_fail));
        // Network / server errors are not encoder errors.
        assert!(!retry_with_x264(Encoder::Nvenc, true, &network));
        assert!(!retry_with_x264(Encoder::Nvenc, true, &[]));
    }

    #[test]
    #[ignore]
    fn probe_finds_working_encoder() {
        let ff = find_ffmpeg().expect("ffmpeg on PATH");
        assert!(probe(&ff, Encoder::X264, 30, 4500));
        assert!(
            probe(&ff, Encoder::Nvenc, 30, 4500),
            "NVENC on the NVIDIA laptop"
        );
        assert!(!probe(&ff, Encoder::Amf, 30, 4500), "no AMD GPU here");
        eprintln!("qsv: {}", probe(&ff, Encoder::Qsv, 30, 4500));
    }

    #[test]
    fn nv12_input_args() {
        let c = StreamConfig {
            pix_fmt: "nv12",
            ..cfg()
        };
        let s = encoder_args(&c, 50123).join(" ");
        assert!(
            s.contains(
                "-f rawvideo -pix_fmt nv12 -colorspace bt709 -color_range tv \
                 -s 1920x1080 -framerate 30 -i pipe:0"
            ),
            "{s}"
        );
        // BGRA input is not tagged (it is RGB) and otherwise identical.
        let b = encoder_args(&cfg(), 50123).join(" ");
        assert!(
            !b.contains("-colorspace") && !b.contains("-color_range"),
            "{b}"
        );
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            norm(b.replace("-pix_fmt bgra", "")),
            norm(
                s.replace("-pix_fmt nv12 -colorspace bt709 -color_range tv", "")
                    .replace(
                        "-colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv",
                        ""
                    )
            )
        );
    }

    #[test]
    fn encoder_args_shape() {
        let a = encoder_args(&cfg(), 50123);
        let s = a.join(" ");
        assert_eq!(s.matches("-analyzeduration 0").count(), 2, "{s}");
        assert!(
            s.contains("-f rawvideo -pix_fmt bgra -s 1920x1080 -framerate 30 -i pipe:0"),
            "{s}"
        );
        assert!(
            s.contains("-f f32le -ar 48000 -ac 2 -i tcp://127.0.0.1:50123"),
            "{s}"
        );
        assert!(
            s.contains("-c:v libx264") && s.contains("-g 60") && s.contains("-b:v 4500k"),
            "{s}"
        );
        assert!(s.contains("-c:a aac -b:a 128k"), "{s}");
        // YouTube recommends B-frames; zerolatency would turn them off.
        assert!(!s.contains("zerolatency") && s.contains("-bf 2"), "{s}");
        assert_eq!(a.last().unwrap(), "pipe:1");
        assert!(!s.contains("abcd-1234-secret"), "{s}");
    }

    #[test]
    fn target_without_key_is_unchanged() {
        let d = Destination {
            site: Site::YouTube,
            key: String::new(),
            url: "rtmp://host/app/k".into(),
        };
        assert_eq!(d.target(), "rtmp://host/app/k");
    }

    #[test]
    fn encoder_writes_mpegts_to_stdout() {
        let a = encoder_args(&cfg_with(vec![]), 5000);
        let n = a.len();
        assert_eq!(
            &a[n - 5..],
            ["-f", "mpegts", "-mpegts_flags", "+resend_headers", "pipe:1"]
        );
        assert!(!a.iter().any(|s| s.starts_with("rtmp")));
        assert!(
            a.windows(2).any(|w| w == ["-g", "60"]),
            "2 s keyframes at 30 fps"
        );
    }

    #[test]
    fn sender_copies_to_flv() {
        let t = "rtmp://live.twitch.tv/app/live_123";
        let a = sender_args(t);
        assert!(a.ends_with(&["-c", "copy", "-f", "flv", t].map(String::from)));
        assert!(a.windows(2).any(|w| w == ["-i", "pipe:0"]));
    }

    #[test]
    fn every_key_is_redacted() {
        let s = "push rtmp://a/live2/yt-key-1 and rtmp://b/app/live_tw_2";
        let r = redact_all(s, &["yt-key-1", "live_tw_2", ""]);
        assert!(!r.contains("yt-key-1") && !r.contains("live_tw_2"));
    }

    #[test]
    fn a_key_inside_another_key_leaves_nothing_visible() {
        let s = "rtmp://a/live2/abc and rtmp://b/app/abcdef";
        for keys in [["abc", "abcdef"], ["abcdef", "abc"]] {
            let r = redact_all(s, &keys);
            assert_eq!(r, "rtmp://a/live2/**** and rtmp://b/app/****", "{keys:?}");
        }
    }

    #[test]
    fn destination_target_appends_the_key() {
        let d = Destination {
            site: Site::Twitch,
            url: "rtmp://live.twitch.tv/app/".into(),
            key: "k".into(),
        };
        assert_eq!(d.target(), "rtmp://live.twitch.tv/app/k");
        assert_eq!(
            (Site::Twitch.as_str(), Site::Twitch.label()),
            ("twitch", "Twitch")
        );
    }

    #[test]
    fn redacts_stream_key() {
        let line =
            "[flv @ 0x1] Failed to connect to rtmp://a.rtmp.youtube.com/live2/abcd-1234-secret";
        let r = redact(line, "abcd-1234-secret");
        assert!(!r.contains("abcd-1234-secret"));
        assert!(r.contains("live2/****"));
        assert_eq!(redact("no key here", ""), "no key here");
    }
}
