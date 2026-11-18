//! Frame <-> model tensor conversion.
use p2c_core::Frame;

/// `frame` (BGRA) resized to `w` x `h`, as RGB floats in 0..1, laid out
/// `[1, 3, h, w]` (all R, then all G, then all B).
pub fn to_rgb_f32_nchw(frame: &Frame, w: u32, h: u32) -> Vec<f32> {
    let scaled;
    let src = if frame.width == w && frame.height == h {
        frame
    } else {
        scaled = p2c_compositor::scale_frame_straight(frame, w, h);
        &scaled
    };
    let plane = (w * h) as usize;
    let mut out = vec![0.0f32; plane * 3];
    for (i, px) in src.data.as_chunks::<4>().0.iter().take(plane).enumerate() {
        out[i] = px[2] as f32 / 255.0;
        out[plane + i] = px[1] as f32 / 255.0;
        out[2 * plane + i] = px[0] as f32 / 255.0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, px: &[[u8; 4]]) -> Frame {
        Frame {
            width: w,
            height: h,
            data: px.iter().flatten().copied().collect(),
        }
    }

    #[test]
    fn nchw_puts_red_first() {
        // BGRA: pure red, pure blue
        let f = frame(2, 1, &[[0, 0, 255, 255], [255, 0, 0, 255]]);
        let t = to_rgb_f32_nchw(&f, 2, 1);
        assert_eq!(t, vec![1.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn nchw_ignores_alpha_when_scaling() {
        // Webcam alpha is unrelated to colour; a 0 alpha must not darken the input.
        let f = frame(4, 2, &[[0, 0, 255, 0]; 8]);
        let t = to_rgb_f32_nchw(&f, 2, 1);
        assert_eq!(&t[0..2], &[1.0, 1.0]);
    }
}
