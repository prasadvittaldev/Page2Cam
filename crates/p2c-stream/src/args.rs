use std::path::PathBuf;

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
}

/// Full publish URL. Contains the key: never log it unredacted.
pub fn target_url(cfg: &StreamConfig) -> String {
    if cfg.key.is_empty() {
        cfg.url.clone()
    } else {
        format!("{}/{}", cfg.url.trim_end_matches('/'), cfg.key)
    }
}

/// ffmpeg command line: raw BGRA video on stdin, f32le stereo audio from a TCP
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
        "bgra",
        "-s",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
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
    for s in [
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-tune",
        "zerolatency",
        "-pix_fmt",
        "yuv420p",
        "-b:v",
        &kbps,
        "-maxrate",
        &kbps,
        "-bufsize",
        &buf,
        "-g",
        &gop,
        "-keyint_min",
        &gop,
        "-sc_threshold",
        "0",
        "-c:a",
        "aac",
        "-b:a",
        "128k",
        "-ar",
        "48000",
        "-f",
        "flv",
    ] {
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
        }
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
