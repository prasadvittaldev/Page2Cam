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

/// Source span `a..b` (non-empty) covered by output index `d` of `n` over `s`.
#[inline]
fn span(d: usize, n: usize, s: usize) -> (usize, usize) {
    let a = d * s / n;
    (a, ((d + 1) * s).div_ceil(n).clamp(a + 1, s))
}

/// Adds the luma of `lrow` under each output column to `ys`. `step` is the
/// cell width when every cell has it (`sw` a multiple of `w`).
fn add_luma(lrow: &[u8], xs: &[(usize, usize)], step: Option<usize>, ys: &mut [u32]) {
    fn fixed<const F: usize>(lrow: &[u8], ys: &mut [u32]) {
        for (s, c) in ys.iter_mut().zip(lrow.as_chunks::<F>().0) {
            *s += c.iter().map(|&p| p as u32).sum::<u32>();
        }
    }
    match step {
        Some(1) => ys.iter_mut().zip(lrow).for_each(|(s, &p)| *s += p as u32),
        Some(2) => fixed::<2>(lrow, ys),
        Some(4) => fixed::<4>(lrow, ys),
        Some(5) => fixed::<5>(lrow, ys),
        _ => {
            for (&(a, b), s) in xs.iter().zip(ys) {
                *s += lrow[a..b].iter().map(|&p| p as u32).sum::<u32>();
            }
        }
    }
}

/// Adds `rows` times the U and V of chroma row `crow` under each output
/// column to `us`/`vs`, each sample counted once per luma column using it.
fn add_chroma(
    crow: &[u8],
    xs: &[(usize, usize)],
    step: Option<usize>,
    rows: u32,
    us: &mut [u32],
    vs: &mut [u32],
) {
    // Even cell width: cells start on a chroma pair and every pair in the
    // cell serves two luma columns.
    fn fixed<const F: usize>(crow: &[u8], rows: u32, us: &mut [u32], vs: &mut [u32]) {
        let w = 2 * rows;
        for ((u, v), c) in us
            .iter_mut()
            .zip(vs.iter_mut())
            .zip(crow.as_chunks::<F>().0)
        {
            let (su, sv) = c
                .as_chunks::<2>()
                .0
                .iter()
                .fold((0u32, 0u32), |(su, sv), p| {
                    (su + p[0] as u32, sv + p[1] as u32)
                });
            *u += w * su;
            *v += w * sv;
        }
    }
    match step {
        Some(2) => fixed::<2>(crow, rows, us, vs),
        Some(4) => fixed::<4>(crow, rows, us, vs),
        _ => {
            for ((&(a, b), su), sv) in xs.iter().zip(us.iter_mut()).zip(vs.iter_mut()) {
                let (mut u, mut v) = (0u32, 0u32);
                let mut sx = a;
                while sx < b {
                    let c = sx & !1;
                    let cols = ((c + 2).min(b) - sx) as u32;
                    u += cols * crow[c] as u32;
                    v += cols * crow[c + 1] as u32;
                    sx = c + 2;
                }
                *su += rows * u;
                *sv += rows * v;
            }
        }
    }
}

/// `frame` (NV12) resized to `w` x `h` (area average, so `nv12_can_feed`
/// must hold), as RGB floats in 0..1 laid out `[1, 3, h, w]`, without a
/// full-size BGRA intermediate. Each output pixel averages Y over the source
/// pixels it covers and U/V over the chroma samples those pixels use (each
/// weighted by how many of them use it), then converts once: BT.601 limited
/// range with [`yuv_to_rgb`]'s coefficients, in floats.
pub fn nv12_to_rgb_f32_nchw(frame: &Nv12Frame, w: u32, h: u32) -> Vec<f32> {
    let (sw, sh) = (frame.width as usize, frame.height as usize);
    let (w, h) = (w as usize, h as usize);
    let (luma, chroma) = frame.data.split_at(sw * sh);
    let plane = w * h;
    let mut out = vec![0.0f32; plane * 3];
    let (r_out, rest) = out.split_at_mut(plane);
    let (g_out, b_out) = rest.split_at_mut(plane);
    let xs: Vec<(usize, usize)> = (0..w).map(|dx| span(dx, w, sw)).collect();
    let step = sw.is_multiple_of(w).then_some(sw / w);
    let inv_x: Vec<f32> = xs.iter().map(|&(a, b)| 1.0 / (b - a) as f32).collect();
    let (mut ys, mut us, mut vs) = (vec![0u32; w], vec![0u32; w], vec![0u32; w]);
    // Coefficients of `yuv_to_rgb` (x/256), with the /255 to 0..1 folded in.
    const MAX: f32 = 255.0 * 256.0;
    for dy in 0..h {
        let (y0, y1) = span(dy, h, sh);
        ys.fill(0);
        us.fill(0);
        vs.fill(0);
        for sy in y0..y1 {
            add_luma(&luma[sy * sw..(sy + 1) * sw], &xs, step, &mut ys);
        }
        // Chroma row r serves luma rows 2r and 2r+1; weight it by how many
        // of those fall in y0..y1.
        for cr in y0 / 2..=(y1 - 1) / 2 {
            let rows = (y1.min(2 * cr + 2) - y0.max(2 * cr)) as u32;
            let crow = &chroma[cr * sw..(cr + 1) * sw];
            add_chroma(crow, &xs, step, rows, &mut us, &mut vs);
        }
        let inv_y = 1.0 / (y1 - y0) as f32;
        let row = dy * w..(dy + 1) * w;
        let outs = r_out[row.clone()]
            .iter_mut()
            .zip(&mut g_out[row.clone()])
            .zip(&mut b_out[row]);
        for (dx, ((r, g), b)) in outs.enumerate() {
            let k = inv_x[dx] * inv_y;
            let c = ys[dx] as f32 * k - 16.0;
            let d = us[dx] as f32 * k - 128.0;
            let e = vs[dx] as f32 * k - 128.0;
            *r = (298.0 * c + 409.0 * e).clamp(0.0, MAX) / MAX;
            *g = (298.0 * c - 100.0 * d - 208.0 * e).clamp(0.0, MAX) / MAX;
            *b = (298.0 * c + 516.0 * d).clamp(0.0, MAX) / MAX;
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

    /// BT.601 limited range in floats (the same coefficients as `yuv_to_rgb`),
    /// written out here so the test does not share code with the conversion.
    fn reference_rgb(y: f32, u: f32, v: f32) -> [f32; 3] {
        let (c, d, e) = (y - 16.0, u - 128.0, v - 128.0);
        [
            (298.0 * c + 409.0 * e) / 256.0,
            (298.0 * c - 100.0 * d - 208.0 * e) / 256.0,
            (298.0 * c + 516.0 * d) / 256.0,
        ]
        .map(|x| x.clamp(0.0, 255.0) / 255.0)
    }

    #[test]
    fn nv12_flat_colour_is_exact() {
        // Skin-ish, saturated blue, near black (clamps), near white (clamps).
        for (y, u, v) in [
            (150u8, 110u8, 160u8),
            (60, 230, 100),
            (10, 128, 128),
            (250, 120, 135),
        ] {
            let (sw, sh) = (1280usize, 720usize);
            let mut data = vec![y; sw * sh];
            data.extend(std::iter::repeat_n([u, v], sw * sh / 4).flatten());
            let n = Nv12Frame::new(sw as u32, sh as u32, data).unwrap();
            let want = reference_rgb(y as f32, u as f32, v as f32);
            for (dw, dh) in [(640usize, 360usize), (256, 144), (1280, 720), (500, 300)] {
                let t = nv12_to_rgb_f32_nchw(&n, dw as u32, dh as u32);
                let plane = dw * dh;
                for k in 0..3 {
                    for (i, &got) in t[k * plane..(k + 1) * plane].iter().enumerate() {
                        assert!(
                            (got - want[k]).abs() < 1e-5,
                            "yuv ({y},{u},{v}) -> {dw}x{dh}: channel {k} pixel {i}: {got} != {}",
                            want[k]
                        );
                    }
                }
            }
        }
    }

    /// The conversion before 2026-11-20: every source pixel converted to RGB,
    /// then averaged. Kept as the reference the new one must stay close to.
    #[allow(clippy::needless_range_loop)] // index math over source spans
    fn previous_nv12_to_rgb_f32_nchw(frame: &Nv12Frame, w: u32, h: u32) -> Vec<f32> {
        let (sw, sh) = (frame.width as usize, frame.height as usize);
        let (w, h) = (w as usize, h as usize);
        let (luma, chroma) = frame.data.split_at(sw * sh);
        let plane = w * h;
        let mut out = vec![0.0f32; plane * 3];
        let xs: Vec<(usize, usize)> = (0..w).map(|dx| span(dx, w, sw)).collect();
        let mut row_sum = vec![[0u32; 3]; w];
        for dy in 0..h {
            let (y0, y1) = span(dy, h, sh);
            row_sum.iter_mut().for_each(|s| *s = [0; 3]);
            for sy in y0..y1 {
                let lrow = &luma[sy * sw..(sy + 1) * sw];
                let crow = &chroma[(sy / 2) * sw..(sy / 2 + 1) * sw];
                for (&(a, b), acc) in xs.iter().zip(row_sum.iter_mut()) {
                    for sx in a..b {
                        let c = sx & !1;
                        let rgb = yuv_to_rgb(lrow[sx], crow[c], crow[c + 1]);
                        for (s, v) in acc.iter_mut().zip(rgb) {
                            *s += v as u32;
                        }
                    }
                }
            }
            for (dx, (&(a, b), acc)) in xs.iter().zip(&row_sum).enumerate() {
                let n = ((y1 - y0) * (b - a)) as f32 * 255.0;
                for (k, s) in acc.iter().enumerate() {
                    out[k * plane + dy * w + dx] = *s as f32 / n;
                }
            }
        }
        out
    }

    /// Gradients plus per-pixel noise (a camera's grain), kept inside the RGB
    /// gamut: where pixels clip, the previous conversion clipped before
    /// averaging and the new one after, so only in-gamut content compares.
    fn noisy_nv12(w: usize, h: usize) -> Nv12Frame {
        let mut seed = 0x2545_f491u32;
        let mut noise = |amp: i32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % (2 * amp as u32 + 1)) as i32 - amp
        };
        let mut data = Vec::with_capacity(w * h * 3 / 2);
        for y in 0..h {
            for x in 0..w {
                data.push((50 + (x * 130 / w + y * 20 / h) as i32 + noise(10)) as u8);
            }
        }
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                data.push((120 + (x * 16 / (w / 2)) as i32 + noise(4)) as u8);
                data.push((120 + (y * 16 / (h / 2)) as i32 + noise(4)) as u8);
            }
        }
        Nv12Frame::new(w as u32, h as u32, data).unwrap()
    }

    #[test]
    fn nv12_input_within_two_levels_of_previous() {
        for n in [synthetic_nv12(1280, 720), noisy_nv12(1280, 720)] {
            for (dw, dh) in [(640u32, 360u32), (256, 144), (500, 300), (1280, 720)] {
                let new = nv12_to_rgb_f32_nchw(&n, dw, dh);
                let old = previous_nv12_to_rgb_f32_nchw(&n, dw, dh);
                let worst = new
                    .iter()
                    .zip(&old)
                    .map(|(a, b)| ((a - b) * 255.0).abs())
                    .fold(0.0f32, f32::max);
                assert!(worst <= 2.0, "->{dw}x{dh}: worst {worst} levels");
            }
        }
    }

    /// New vs previous conversion, interleaved so machine load hits both.
    /// `CARGO_PROFILE_DEV_OPT_LEVEL=3 cargo test -p p2c-segment nv12_input_timing -- --ignored --nocapture`
    #[test]
    #[ignore = "timing"]
    fn nv12_input_timing() {
        let n = noisy_nv12(1280, 720);
        for (dw, dh) in [(640u32, 360u32), (256, 144)] {
            let (mut new, mut old) = (
                p2c_core::DurationStats::default(),
                p2c_core::DurationStats::default(),
            );
            for i in 0..105 {
                let t = std::time::Instant::now();
                std::hint::black_box(nv12_to_rgb_f32_nchw(&n, dw, dh));
                let a = t.elapsed();
                let t = std::time::Instant::now();
                std::hint::black_box(previous_nv12_to_rgb_f32_nchw(&n, dw, dh));
                let b = t.elapsed();
                if i >= 5 {
                    new.record(a);
                    old.record(b);
                }
            }
            println!(
                "NV12 1280x720 -> {dw}x{dh} new:      {}",
                new.summary().unwrap()
            );
            println!(
                "NV12 1280x720 -> {dw}x{dh} previous: {}",
                old.summary().unwrap()
            );
        }
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
            // The direct path skips the integer rounding: at most half a level.
            assert!(
                ((a - b) * 255.0).abs() <= 0.501,
                "{} vs {}",
                a * 255.0,
                b * 255.0
            );
        }
    }
}
