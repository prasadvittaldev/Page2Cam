//! Frame <-> model tensor conversion.
use p2c_core::Frame;

/// `frame` (BGRA) resized to `w` x `h`, as RGB floats in 0..1, laid out
/// `[1, 3, h, w]` (all R, then all G, then all B).
pub fn to_rgb_f32_nchw(frame: &Frame, w: u32, h: u32) -> Vec<f32> {
    let scaled;
    let src = if frame.width == w && frame.height == h {
        frame
    } else {
        scaled = p2c_compositor::scale_frame(frame, w, h);
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

/// Write a `mw` x `mh` mask (0..1, row-major) into the alpha byte of every
/// pixel of `frame`, scaled bilinearly to the frame size. BGR is untouched.
pub fn write_alpha(frame: &mut Frame, mask: &[f32], mw: u32, mh: u32) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (mw, mh) = (mw as usize, mh as usize);
    if w == 0 || h == 0 || mw == 0 || mh == 0 || mask.len() < mw * mh {
        return;
    }
    // Corner-aligned mapping: frame edges sample mask edges exactly.
    let sx = if w > 1 { (mw - 1) as f32 / (w - 1) as f32 } else { 0.0 };
    let sy = if h > 1 { (mh - 1) as f32 / (h - 1) as f32 } else { 0.0 };
    let cols: Vec<(usize, usize, f32)> = (0..w)
        .map(|x| {
            let fx = x as f32 * sx;
            let x0 = (fx as usize).min(mw - 1);
            (x0, (x0 + 1).min(mw - 1), fx - x0 as f32)
        })
        .collect();
    for y in 0..h {
        let fy = y as f32 * sy;
        let y0 = (fy as usize).min(mh - 1);
        let y1 = (y0 + 1).min(mh - 1);
        let ty = fy - y0 as f32;
        let (r0, r1) = (&mask[y0 * mw..][..mw], &mask[y1 * mw..][..mw]);
        let row = &mut frame.data[y * w * 4..][..w * 4];
        for (x, &(x0, x1, tx)) in cols.iter().enumerate() {
            let top = r0[x0] + (r0[x1] - r0[x0]) * tx;
            let bot = r1[x0] + (r1[x1] - r1[x0]) * tx;
            let v = top + (bot - top) * ty;
            row[x * 4 + 3] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
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
    fn write_alpha_scales_mask() {
        let mut f = frame(640, 480, &vec![[10, 20, 30, 255]; 640 * 480]);
        write_alpha(&mut f, &[0.0, 1.0], 2, 1);
        let a = |x: usize, y: usize| f.data[(y * 640 + x) * 4 + 3];
        assert_eq!(a(0, 0), 0);
        assert_eq!(a(639, 479), 255);
        assert!((a(320, 240) as i32 - 128).abs() <= 2, "{}", a(320, 240));
        assert_eq!(&f.data[0..3], &[10, 20, 30]);
    }

    #[test]
    fn write_alpha_tiny_frame() {
        let mut f = frame(1, 1, &[[0, 0, 0, 0]]);
        write_alpha(&mut f, &vec![1.0; 256 * 144], 256, 144);
        assert_eq!(f.data[3], 255);
    }

    #[test]
    fn mask_values_clamped() {
        let mut f = frame(2, 1, &[[0; 4], [0; 4]]);
        write_alpha(&mut f, &[-0.5, 1.7], 2, 1);
        assert_eq!((f.data[3], f.data[7]), (0, 255));
    }
}
