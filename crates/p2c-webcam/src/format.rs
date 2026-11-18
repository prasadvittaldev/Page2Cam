//! Choosing the webcam mode: smooth first, then size, then cheapest decode.
use nokhwa::utils::{CameraFormat, FrameFormat};

/// Pick the mode to request from `formats`:
/// 1. at least 25 fps (else the highest fps on offer),
/// 2. resolution closest to 1280x720 by pixel count, never above 1920x1080,
/// 3. MJPEG, then YUYV, then NV12; formats we cannot decode are skipped.
pub fn choose_format(formats: &[CameraFormat]) -> Option<CameraFormat> {
    let decodable: Vec<CameraFormat> = formats
        .iter()
        .copied()
        .filter(|f| format_rank(f.format()).is_some())
        .collect();
    let small: Vec<CameraFormat> = decodable
        .iter()
        .copied()
        .filter(|f| f.width() <= 1920 && f.height() <= 1080)
        .collect();
    let pool = if small.is_empty() { decodable } else { small };
    let max_fps = pool.iter().map(|f| f.frame_rate()).max()?;
    let fps_floor = if max_fps >= 25 { 25 } else { max_fps };
    let target = 1280i64 * 720;
    pool.into_iter()
        .filter(|f| f.frame_rate() >= fps_floor)
        .min_by_key(|f| {
            let px = f.width() as i64 * f.height() as i64;
            (
                (px - target).abs(),
                format_rank(f.format()),
                u32::MAX - f.frame_rate(),
            )
        })
}

/// One line listing a camera's modes, grouped by format and size, e.g.
/// "MJPEG 1280x720 @15/30, YUYV 640x480 @30" (for the log).
pub fn describe_modes(formats: &[CameraFormat]) -> String {
    let mut groups: Vec<(FrameFormat, u32, u32, Vec<u32>)> = Vec::new();
    for f in formats {
        let key = (f.format(), f.width(), f.height());
        match groups.iter_mut().find(|g| (g.0, g.1, g.2) == key) {
            Some(g) => g.3.push(f.frame_rate()),
            None => groups.push((key.0, key.1, key.2, vec![f.frame_rate()])),
        }
    }
    groups
        .into_iter()
        .map(|(fmt, w, h, mut fps)| {
            fps.sort_unstable();
            fps.dedup();
            let fps: Vec<String> = fps.iter().map(u32::to_string).collect();
            format!("{fmt} {w}x{h} @{}", fps.join("/"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Lower is better; None = we cannot decode it.
fn format_rank(f: FrameFormat) -> Option<u8> {
    match f {
        FrameFormat::MJPEG => Some(0),
        FrameFormat::YUYV => Some(1),
        FrameFormat::NV12 => Some(2),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nokhwa::utils::Resolution;

    fn f(w: u32, h: u32, fmt: FrameFormat, fps: u32) -> CameraFormat {
        CameraFormat::new(Resolution::new(w, h), fmt, fps)
    }

    #[test]
    fn prefers_30fps_over_resolution() {
        let got = choose_format(&[
            f(1920, 1080, FrameFormat::MJPEG, 5),
            f(1280, 720, FrameFormat::MJPEG, 30),
        ]);
        assert_eq!(got, Some(f(1280, 720, FrameFormat::MJPEG, 30)));
    }

    #[test]
    fn prefers_mjpeg_at_equal_mode() {
        let got = choose_format(&[
            f(1280, 720, FrameFormat::YUYV, 30),
            f(1280, 720, FrameFormat::MJPEG, 30),
            f(1280, 720, FrameFormat::NV12, 30),
        ]);
        assert_eq!(got, Some(f(1280, 720, FrameFormat::MJPEG, 30)));
    }

    #[test]
    fn prefers_closest_to_720p_and_never_above_1080p() {
        let got = choose_format(&[
            f(3840, 2160, FrameFormat::MJPEG, 30),
            f(640, 480, FrameFormat::MJPEG, 30),
            f(1920, 1080, FrameFormat::MJPEG, 30),
        ]);
        assert_eq!(got, Some(f(640, 480, FrameFormat::MJPEG, 30)));
    }

    #[test]
    fn skips_unsupported_formats() {
        let got = choose_format(&[
            f(1280, 720, FrameFormat::GRAY, 30),
            f(640, 480, FrameFormat::YUYV, 30),
        ]);
        assert_eq!(got, Some(f(640, 480, FrameFormat::YUYV, 30)));
    }

    #[test]
    fn falls_back_to_highest_fps_when_none_reach_25() {
        let got = choose_format(&[
            f(1280, 720, FrameFormat::MJPEG, 10),
            f(640, 480, FrameFormat::MJPEG, 15),
        ]);
        assert_eq!(got, Some(f(640, 480, FrameFormat::MJPEG, 15)));
    }

    #[test]
    fn modes_are_summarized_compactly() {
        let s = describe_modes(&[
            f(1280, 720, FrameFormat::MJPEG, 30),
            f(1280, 720, FrameFormat::MJPEG, 15),
            f(1280, 720, FrameFormat::MJPEG, 30),
            f(640, 480, FrameFormat::YUYV, 30),
        ]);
        assert_eq!(s, "MJPEG 1280x720 @15/30, YUYV 640x480 @30");
    }

    #[test]
    fn empty_list_none() {
        assert_eq!(choose_format(&[]), None);
        assert_eq!(choose_format(&[f(1280, 720, FrameFormat::GRAY, 30)]), None);
    }
}
