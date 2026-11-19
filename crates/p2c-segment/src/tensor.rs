//! Frame <-> model tensor conversion.
use p2c_core::{Frame, Nv12Frame};

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

/// BT.601 limited range YUV -> RGB bytes. The same integer maths as the
/// webcam crate's BGRA decode, so both inputs give the model the same colours.
#[inline]
fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [i32; 3] {
    let c = y as i32 - 16;
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    let clamp = |x: i32| x.clamp(0, 255);
    [
        clamp((298 * c + 409 * e + 128) >> 8),
        clamp((298 * c - 100 * d - 208 * e + 128) >> 8),
        clamp((298 * c + 516 * d + 128) >> 8),
    ]
}

/// True when [`nv12_to_rgb_f32_nchw`] can serve a `w` x `h` input from this
/// frame: it only averages down (a smaller frame goes the BGRA way).
pub fn nv12_can_feed(frame: &Nv12Frame, w: u32, h: u32) -> bool {
    frame.width >= w && frame.height >= h
}

/// `frame` (NV12) resized to `w` x `h` (area average, so `nv12_can_feed`
/// must hold), as RGB floats in 0..1 laid out `[1, 3, h, w]`, without a
/// full-size BGRA intermediate. Each output pixel averages the converted RGB
/// of the source pixels it covers.
#[allow(clippy::needless_range_loop)] // index math over source spans
pub fn nv12_to_rgb_f32_nchw(frame: &Nv12Frame, w: u32, h: u32) -> Vec<f32> {
    let (sw, sh) = (frame.width as usize, frame.height as usize);
    let (w, h) = (w as usize, h as usize);
    let (luma, chroma) = frame.data.split_at(sw * sh);
    let plane = w * h;
    let mut out = vec![0.0f32; plane * 3];
    // Source column span of every output column.
    let xs: Vec<(usize, usize)> = (0..w)
        .map(|dx| {
            let a = dx * sw / w;
            (a, ((dx + 1) * sw).div_ceil(w).clamp(a + 1, sw))
        })
        .collect();
    let mut row_sum = vec![[0u32; 3]; w];
    for dy in 0..h {
        let (y0, y1) = (
            dy * sh / h,
            ((dy + 1) * sh).div_ceil(h).clamp(dy * sh / h + 1, sh),
        );
        row_sum.iter_mut().for_each(|s| *s = [0; 3]);
        for sy in y0..y1 {
            let lrow = &luma[sy * sw..(sy + 1) * sw];
            let crow = &chroma[(sy / 2) * sw..(sy / 2 + 1) * sw];
            for (&(a, b), acc) in xs.iter().zip(row_sum.iter_mut()) {
                for sx in a..b {
                    let c = sx & !1;
                    let rgb = yuv_to_rgb(lrow[sx], crow[c], crow[c + 1]);
                    for k in 0..3 {
                        acc[k] += rgb[k] as u32;
                    }
                }
            }
        }
        for (dx, (&(a, b), acc)) in xs.iter().zip(&row_sum).enumerate() {
            let n = ((y1 - y0) * (b - a)) as f32 * 255.0;
            let i = dy * w + dx;
            for k in 0..3 {
                out[k * plane + i] = acc[k] as f32 / n;
            }
        }
    }
    out
}

/// Full-size BGRA copy of an NV12 frame (the way the webcam crate decodes
/// it). Only for engines that cannot take NV12 directly.
pub fn nv12_to_frame(n: &Nv12Frame) -> Frame {
    let (w, h) = (n.width as usize, n.height as usize);
    let (luma, chroma) = n.data.split_at(w * h);
    let mut data = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let c = (y / 2) * w + (x / 2) * 2;
            let [r, g, b] = yuv_to_rgb(luma[y * w + x], chroma[c], chroma[c + 1]);
            data.extend_from_slice(&[b as u8, g as u8, r as u8, 255]);
        }
    }
    Frame {
        width: n.width,
        height: n.height,
        data,
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
    fn nchw_ignores_alpha_when_scaling() {
        // Webcam alpha is unrelated to colour; a 0 alpha must not darken the input.
        let f = frame(4, 2, &[[0, 0, 255, 0]; 8]);
        let t = to_rgb_f32_nchw(&f, 2, 1);
        assert_eq!(&t[0..2], &[1.0, 1.0]);
    }

    /// Smooth synthetic NV12 frame (gradients in luma and chroma).
    fn synthetic_nv12(w: usize, h: usize) -> Nv12Frame {
        let mut data = Vec::with_capacity(w * h * 3 / 2);
        for y in 0..h {
            for x in 0..w {
                data.push((40 + x * 160 / w + y * 40 / h) as u8);
            }
        }
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                data.push((100 + x * 50 / (w / 2)) as u8);
                data.push((110 + y * 50 / (h / 2)) as u8);
            }
        }
        Nv12Frame::new(w as u32, h as u32, data).unwrap()
    }

    #[test]
    fn nv12_model_input_matches_bgra_path() {
        for (sw, sh, dw, dh) in [
            (1280, 720, 640, 360),
            (1280, 720, 256, 144),
            (640, 360, 640, 360),
        ] {
            let n = synthetic_nv12(sw, sh);
            assert!(nv12_can_feed(&n, dw as u32, dh as u32));
            let direct = nv12_to_rgb_f32_nchw(&n, dw as u32, dh as u32);
            let via_bgra = to_rgb_f32_nchw(&nv12_to_frame(&n), dw as u32, dh as u32);
            assert_eq!(direct.len(), via_bgra.len());
            let worst = direct
                .iter()
                .zip(&via_bgra)
                .map(|(a, b)| ((a - b) * 255.0).abs())
                .fold(0.0f32, f32::max);
            assert!(worst <= 2.0, "{sw}x{sh}->{dw}x{dh}: worst {worst} levels");
        }
        // Too small to average down: the caller must use the BGRA path.
        assert!(!nv12_can_feed(&synthetic_nv12(320, 180), 640, 360));
    }
}

#[cfg(test)]
mod webcam_equivalence {
    use super::*;
    use p2c_webcam::{to_bgra, RawFormat};

    #[test]
    fn nv12_input_equals_webcam_decode_at_same_size() {
        let (w, h) = (64usize, 36usize);
        let mut data: Vec<u8> = (0..w * h).map(|i| (16 + i * 13 % 220) as u8).collect();
        data.extend((0..w * h / 2).map(|i| (40 + i * 7 % 180) as u8));
        let n = Nv12Frame::new(w as u32, h as u32, data.clone()).unwrap();
        let bgra = to_bgra(RawFormat::Nv12, w as u32, h as u32, &data).unwrap();
        assert_eq!(nv12_to_frame(&n).data, bgra.data, "converted pixels differ");
        let direct = nv12_to_rgb_f32_nchw(&n, w as u32, h as u32);
        let via = to_rgb_f32_nchw(&bgra, w as u32, h as u32);
        for (a, b) in direct.iter().zip(&via) {
            assert!(((a - b) * 255.0).abs() < 0.01);
        }
    }
}
