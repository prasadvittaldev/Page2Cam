//! Guided-filter upsampling of a model-size person mask (He et al., "fast guided filter").
//!
//! The webcam luma is the guide: where the guide has an edge, the mask edge
//! snaps to it, so a 256x144 mask gets a crisp outline at output size.
use crate::scale_frame_straight;
use p2c_core::{Frame, Mask};

/// Subsampling factor for the statistics (the filter itself runs at 1/S size).
const SUBSAMPLE: u32 = 4;

/// Bilinear resample of an `f32` plane (pixel-centre aligned). Never reads out of bounds.
fn resample(src: &[f32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; dw * dh];
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return out;
    }
    let xs: Vec<(usize, usize, f32)> = (0..dw).map(|x| taps(x, dw, sw)).collect();
    for y in 0..dh {
        let (y0, y1, fy) = taps(y, dh, sh);
        let (r0, r1) = (&src[y0 * sw..(y0 + 1) * sw], &src[y1 * sw..(y1 + 1) * sw]);
        let row = &mut out[y * dw..(y + 1) * dw];
        for (o, &(x0, x1, fx)) in row.iter_mut().zip(&xs) {
            let top = r0[x0] + (r0[x1] - r0[x0]) * fx;
            let bot = r1[x0] + (r1[x1] - r1[x0]) * fx;
            *o = top + (bot - top) * fy;
        }
    }
    out
}

fn taps(d: usize, dn: usize, sn: usize) -> (usize, usize, f32) {
    let c = ((d as f32 + 0.5) * sn as f32 / dn as f32 - 0.5).clamp(0.0, (sn - 1) as f32);
    let i0 = c.floor() as usize;
    (i0, (i0 + 1).min(sn - 1), c - i0 as f32)
}

/// Mean over a (2r+1)^2 window, clamped at the borders (divides by the real count).
fn box_mean(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let out = &mut tmp[y * w..(y + 1) * w];
        let mut sum: f64 = row[..(r + 1).min(w)].iter().map(|&v| v as f64).sum();
        for x in 0..w {
            let (lo, hi) = (x.saturating_sub(r), (x + r + 1).min(w));
            out[x] = (sum / (hi - lo) as f64) as f32;
            if x + r + 1 < w {
                sum += row[x + r + 1] as f64;
            }
            if x >= r {
                sum -= row[x - r] as f64;
            }
        }
    }
    let mut out = vec![0.0f32; w * h];
    let mut sums: Vec<f64> = (0..w)
        .map(|x| (0..(r + 1).min(h)).map(|y| tmp[y * w + x] as f64).sum())
        .collect();
    for y in 0..h {
        let (lo, hi) = (y.saturating_sub(r), (y + r + 1).min(h));
        let n = (hi - lo) as f64;
        for x in 0..w {
            out[y * w + x] = (sums[x] / n) as f32;
        }
        if y + r + 1 < h {
            for x in 0..w {
                sums[x] += tmp[(y + r + 1) * w + x] as f64;
            }
        }
        if y >= r {
            for x in 0..w {
                sums[x] -= tmp[(y - r) * w + x] as f64;
            }
        }
    }
    out
}

/// Rec.601 luma of a BGRA pixel, 0..255.
fn luma8(px: &[u8; 4]) -> u8 {
    ((29 * px[0] as u32 + 150 * px[1] as u32 + 77 * px[2] as u32) >> 8) as u8
}

/// Mean luma (0..1) of the `s` x `s` blocks of an 8-bit plane, one value per block
/// (edge blocks use the pixels that exist).
fn block_means(plane: &[u8], w: usize, h: usize, lw: usize, lh: usize, s: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; lw * lh];
    for ly in 0..lh {
        let (y0, y1) = (ly * s, ((ly + 1) * s).min(h));
        for lx in 0..lw {
            let (x0, x1) = (lx * s, ((lx + 1) * s).min(w));
            let mut sum = 0u32;
            for y in y0..y1 {
                sum += plane[y * w + x0..y * w + x1]
                    .iter()
                    .map(|&v| v as u32)
                    .sum::<u32>();
            }
            out[ly * lw + lx] = sum as f32 / (((y1 - y0) * (x1 - x0)) as f32 * 255.0);
        }
    }
    out
}

/// Refine `mask` to `out_w` x `out_h` alpha (0..255) guided by the webcam luma.
///
/// `radius` is the window radius in output pixels (applied at 1/4 size);
/// `eps` regularises the guide variance (guide and mask in 0..1).
/// The guide may have any size; it is scaled to the output size.
pub fn guided_upsample(
    mask: &Mask,
    guide: &Frame,
    out_w: u32,
    out_h: u32,
    radius: u32,
    eps: f32,
) -> Vec<u8> {
    let (ow, oh) = (out_w as usize, out_h as usize);
    let mut out = vec![0u8; ow * oh];
    if ow == 0
        || oh == 0
        || mask.width == 0
        || mask.height == 0
        || guide.width == 0
        || guide.height == 0
        || mask.data.len() != mask.width as usize * mask.height as usize
        || guide.data.len() != guide.width as usize * guide.height as usize * 4
    {
        return out;
    }
    let (lw, lh) = (
        out_w.div_ceil(SUBSAMPLE).max(1) as usize,
        out_h.div_ceil(SUBSAMPLE).max(1) as usize,
    );
    let r = ((radius / SUBSAMPLE) as usize).max(1);

    // Guide at output size (for the final pass) and at low size (for the statistics).
    let full;
    let full_ref = if (guide.width, guide.height) == (out_w, out_h) {
        guide
    } else {
        full = scale_frame_straight(guide, out_w, out_h);
        &full
    };
    let luma_full: Vec<u8> = full_ref.data.as_chunks::<4>().0.iter().map(luma8).collect();
    let i_low = block_means(&luma_full, ow, oh, lw, lh, SUBSAMPLE as usize);
    let mask_f: Vec<f32> = mask.data.iter().map(|&v| v as f32 / 255.0).collect();
    let p_low = resample(&mask_f, mask.width as usize, mask.height as usize, lw, lh);

    let ip: Vec<f32> = i_low.iter().zip(&p_low).map(|(i, p)| i * p).collect();
    let ii: Vec<f32> = i_low.iter().map(|i| i * i).collect();
    let mean_i = box_mean(&i_low, lw, lh, r);
    let mean_p = box_mean(&p_low, lw, lh, r);
    let corr_ip = box_mean(&ip, lw, lh, r);
    let corr_ii = box_mean(&ii, lw, lh, r);

    let n = lw * lh;
    let mut a = vec![0.0f32; n];
    let mut b = vec![0.0f32; n];
    for k in 0..n {
        let var = (corr_ii[k] - mean_i[k] * mean_i[k]).max(0.0);
        let cov = corr_ip[k] - mean_i[k] * mean_p[k];
        a[k] = cov / (var + eps);
        b[k] = mean_p[k] - a[k] * mean_i[k];
    }
    let ma = box_mean(&a, lw, lh, r);
    let mb = box_mean(&b, lw, lh, r);

    // Fused final pass: upsample mean_a / mean_b bilinearly and apply q = a*I + b.
    // Rows are blended vertically once, then each pixel needs one horizontal lerp each.
    // Large outputs are split into row bands across a few threads.
    let xs: Vec<(usize, usize, f32)> = (0..ow).map(|x| taps(x, ow, lw)).collect();
    let bands = if ow * oh >= 1 << 16 {
        std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .clamp(1, 4)
    } else {
        1
    };
    let band_rows = oh.div_ceil(bands);
    let band = |first_row: usize, dst: &mut [u8]| {
        let (mut ra, mut rb) = (vec![0.0f32; lw], vec![0.0f32; lw]);
        for (dy, drow) in dst.chunks_mut(ow).enumerate() {
            let y = first_row + dy;
            let (y0, y1, fy) = taps(y, oh, lh);
            for x in 0..lw {
                let (a0, a1) = (ma[y0 * lw + x], ma[y1 * lw + x]);
                let (b0, b1) = (mb[y0 * lw + x], mb[y1 * lw + x]);
                ra[x] = a0 + (a1 - a0) * fy;
                rb[x] = b0 + (b1 - b0) * fy;
            }
            let src = &luma_full[y * ow..(y + 1) * ow];
            for ((o, &l), &(x0, x1, fx)) in drow.iter_mut().zip(src).zip(&xs) {
                let a = ra[x0] + (ra[x1] - ra[x0]) * fx;
                let b = rb[x0] + (rb[x1] - rb[x0]) * fx;
                let q = a * (l as f32 * (1.0 / 255.0)) + b;
                *o = (q * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
    };
    if bands == 1 {
        band(0, &mut out);
    } else {
        std::thread::scope(|sc| {
            for (i, chunk) in out.chunks_mut(band_rows * ow).enumerate() {
                let band = &band;
                sc.spawn(move || band(i * band_rows, chunk));
            }
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 40 output px = 10 px at the 1/4-size filter resolution.
    const R: u32 = 40;
    const EPS: f32 = 1e-4;

    fn step_guide(w: u32, h: u32, edge: u32) -> Frame {
        let mut data = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < edge { 40 } else { 220 };
                data.extend_from_slice(&[v, v, v, 255]);
            }
        }
        Frame::from_bgra(w, h, data).unwrap()
    }

    /// Mask of size mw x mh whose value ramps (blurry) across the edge.
    fn blurry_mask(mw: u32, mh: u32, edge_frac: f32, ramp: f32) -> Mask {
        let mut data = Vec::new();
        for _ in 0..mh {
            for x in 0..mw {
                let t = ((x as f32 + 0.5) / mw as f32 - edge_frac) / ramp;
                data.push((t.clamp(-0.5, 0.5) * 255.0 + 127.5) as u8);
            }
        }
        Mask::new(mw, mh, data).unwrap()
    }

    #[test]
    fn guided_keeps_hard_edge_sharp() {
        let (w, h) = (640u32, 360u32);
        let guide = step_guide(w, h, 320);
        let mask = blurry_mask(160, 90, 0.5, 0.03);
        let out = guided_upsample(&mask, &guide, w, h, R, EPS);
        let row = &out[(h as usize / 2) * w as usize..][..w as usize];
        let lo = row.iter().position(|&v| v >= 26).unwrap();
        let hi = row.iter().position(|&v| v >= 230).unwrap();
        assert!(hi - lo <= 3, "10-90% transition {} px", hi - lo);
        assert!(row[10] < 10 && row[w as usize - 10] > 245);
    }

    #[test]
    fn guided_flat_stays_flat() {
        let guide = Frame::from_bgra(64, 36, [90u8, 90, 90, 255].repeat(64 * 36)).unwrap();
        for v in [0u8, 255, 128] {
            let mask = Mask::new(16, 9, vec![v; 144]).unwrap();
            let out = guided_upsample(&mask, &guide, 64, 36, R, EPS);
            assert!(out.iter().all(|&o| o.abs_diff(v) <= 1), "v={v}");
        }
    }

    #[test]
    fn guided_handles_any_mask_and_guide_sizes() {
        let mask = Mask::new(256, 144, (0..256 * 144).map(|i| (i % 251) as u8).collect()).unwrap();
        for (gw, gh) in [(1280, 720), (1, 1), (7, 5), (333, 201)] {
            let guide = Frame::new_black(gw, gh);
            for (ow, oh) in [(1280, 720), (1, 1), (3, 2), (257, 143), (960, 540)] {
                let out = guided_upsample(&mask, &guide, ow, oh, R, EPS);
                assert_eq!(
                    out.len(),
                    ow as usize * oh as usize,
                    "{gw}x{gh} -> {ow}x{oh}"
                );
            }
        }
        let tiny = Mask::new(1, 1, vec![200]).unwrap();
        let out = guided_upsample(&tiny, &Frame::new_black(10, 10), 9, 9, R, EPS);
        assert_eq!(out.len(), 81);
        assert!(guided_upsample(&mask, &Frame::new_black(4, 4), 0, 5, R, EPS).is_empty());
    }

    #[test]
    #[ignore = "speed measurement; run with --release --nocapture"]
    fn guided_speed_1280x720() {
        let guide = step_guide(1280, 720, 600);
        let mask = blurry_mask(256, 144, 0.47, 0.1);
        let _ = guided_upsample(&mask, &guide, 1280, 720, R, EPS);
        let n = 20;
        let t = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(guided_upsample(&mask, &guide, 1280, 720, R, EPS));
        }
        println!(
            "guided_upsample 1280x720: {:.2} ms",
            t.elapsed().as_secs_f64() * 1000.0 / n as f64
        );
    }
}
