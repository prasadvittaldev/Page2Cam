use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use p2c_core::{CHANNELS, SAMPLE_RATE};

#[derive(Clone, Debug)]
pub struct StreamConfig {
    /// RTMP server URL (e.g. rtmp://a.rtmp.youtube.com/live2).
    pub url: String,
    /// Stream key, appended as the last path segment. Empty: `url` is complete.
    pub key: String,
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

/// Whether `e` really encodes here: runs ffmpeg on one black frame to a null
/// output (an `-encoders` listing says nothing about the driver or the GPU).
/// Gives up after 10 s.
pub fn probe(ffmpeg: &Path, e: Encoder) -> bool {
    let mut cmd = Command::new(ffmpeg);
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=256x144:r=30",
        "-frames:v",
        "1",
        "-c:v",
        e.ffmpeg_name(),
        "-f",
        "null",
        "-",
    ])
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

/// Full publish URL. Contains the key: never log it unredacted.
pub fn target_url(cfg: &StreamConfig) -> String {
    if cfg.key.is_empty() {
        cfg.url.clone()
    } else {
        format!("{}/{}", cfg.url.trim_end_matches('/'), cfg.key)
    }
}

/// ffmpeg command line: raw BGRA or NV12 video on stdin, f32le stereo audio from a TCP
/// socket we listen on, H.264 + AAC in FLV to the RTMP target.
pub fn ffmpeg_args(cfg: &StreamConfig, audio_port: u16) -> Vec<String> {
    let gop = (cfg.fps * 2).to_string();
    let kbps = format!("{}k", cfg.video_kbps);
    let buf = format!("{}k", cfg.video_kbps * 2);
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
    let rate = |a: &mut Vec<String>, with_buf: bool| {
        a.extend(["-b:v", &kbps, "-maxrate", &kbps].map(String::from));
        if with_buf {
            a.extend(["-bufsize", &buf].map(String::from));
        }
    };
    match cfg.encoder {
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
    for s in ["-c:a", "aac", "-b:a", "128k", "-ar", "48000", "-f", "flv"] {
        a.push(s.to_string());
    }
    a.push(target_url(cfg));
    a
}

/// Replace every occurrence of the stream key with `****`.
pub fn redact(s: &str, key: &str) -> String {
    if key.is_empty() {
        s.to_string()
    } else {
        s.replace(key, "****")
    }
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
        StreamConfig {
            url: "rtmp://a.rtmp.youtube.com/live2/".into(),
            key: "abcd-1234-secret".into(),
            width: 1920,
            height: 1080,
            fps: 30,
            video_kbps: 4500,
            pix_fmt: "bgra",
            encoder: Encoder::X264,
        }
    }

    fn with(e: Encoder) -> String {
        ffmpeg_args(
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
            let s = ffmpeg_args(
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

    #[test]
    #[ignore]
    fn probe_finds_working_encoder() {
        let ff = find_ffmpeg().expect("ffmpeg on PATH");
        assert!(probe(&ff, Encoder::X264));
        assert!(probe(&ff, Encoder::Nvenc), "NVENC on the NVIDIA laptop");
        assert!(!probe(&ff, Encoder::Amf), "no AMD GPU here");
        eprintln!("qsv: {}", probe(&ff, Encoder::Qsv));
    }

    #[test]
    fn nv12_input_args() {
        let c = StreamConfig {
            pix_fmt: "nv12",
            ..cfg()
        };
        let s = ffmpeg_args(&c, 50123).join(" ");
        assert!(
            s.contains(
                "-f rawvideo -pix_fmt nv12 -colorspace bt709 -color_range tv \
                 -s 1920x1080 -framerate 30 -i pipe:0"
            ),
            "{s}"
        );
        // BGRA input is not tagged (it is RGB) and otherwise identical.
        let b = ffmpeg_args(&cfg(), 50123).join(" ");
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
    fn ffmpeg_args_shape() {
        let a = ffmpeg_args(&cfg(), 50123);
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
        assert_eq!(
            a.last().unwrap(),
            "rtmp://a.rtmp.youtube.com/live2/abcd-1234-secret"
        );
        assert_eq!(a[a.len() - 2], "flv");
    }

    #[test]
    fn target_url_without_key_is_unchanged() {
        let c = StreamConfig {
            key: String::new(),
            url: "rtmp://host/app/k".into(),
            ..cfg()
        };
        assert_eq!(target_url(&c), "rtmp://host/app/k");
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
