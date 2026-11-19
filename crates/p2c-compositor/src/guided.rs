//! Guided-filter upsampling of a model-size person mask (He et al., "fast guided filter").
//!
//! The webcam luma is the guide: where the guide has an edge, the mask edge
//! snaps to it, so a 256x144 mask gets a crisp outline at output size.
//! Single-threaded; keep a [`GuidedScratch`] to avoid per-call allocations.
use crate::compose::resize_into;
use fast_image_resize::Resizer;
use p2c_core::{Frame, Mask};

/// Subsampling factor for the statistics (the filter itself runs at 1/S size).
pub const GUIDED_SUBSAMPLE: usize = 4;
const SUBSAMPLE: usize = GUIDED_SUBSAMPLE;

/// Source coordinate for output pixel `d` when the source grid has `sn` cells of
/// `s` output pixels each (cell `i` is centred at `(i + 0.5) * s`).
fn taps_cells(d: usize, s: usize, sn: usize) -> (usize, usize, f32) {
    let c = ((d as f32 + 0.5) / s as f32 - 0.5).clamp(0.0, (sn - 1) as f32);
    let i0 = c.floor() as usize;
    (i0, (i0 + 1).min(sn - 1), c - i0 as f32)
}

/// Mean over a (2r+1)^2 window, clamped at the borders (divides by the real count).
fn box_mean(
    src: &[f32],
    w: usize,
    h: usize,
    r: usize,
    out: &mut [f32],
    tmp: &mut [f32],
    sums: &mut Vec<f64>,
) {
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let o = &mut tmp[y * w..(y + 1) * w];
        let mut sum: f64 = row[..(r + 1).min(w)].iter().map(|&v| v as f64).sum();
        for x in 0..w {
            let (lo, hi) = (x.saturating_sub(r), (x + r + 1).min(w));
            o[x] = (sum / (hi - lo) as f64) as f32;
            if x + r + 1 < w {
                sum += row[x + r + 1] as f64;
            }
            if x >= r {
                sum -= row[x - r] as f64;
            }
        }
    }
    sums.clear();
    sums.extend((0..w).map(|x| {
        (0..(r + 1).min(h))
            .map(|y| tmp[y * w + x] as f64)
            .sum::<f64>()
    }));
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
}

/// Rec.601 luma of a BGRA pixel, 0..255.
fn luma8(px: &[u8; 4]) -> u8 {
    ((29 * px[0] as u32 + 150 * px[1] as u32 + 77 * px[2] as u32) >> 8) as u8
}

/// Reusable buffers for [`guided_upsample`]; keep one per output stream.
#[derive(Default)]
pub struct GuidedScratch {
    resizer: Option<Resizer>,
    scaled: Vec<u8>,
    luma: Vec<u8>,
    i_low: Vec<f32>,
    p_low: Vec<f32>,
    ip: Vec<f32>,
    ii: Vec<f32>,
    mean_i: Vec<f32>,
    mean_p: Vec<f32>,
    corr_ip: Vec<f32>,
    corr_ii: Vec<f32>,
    a: Vec<f32>,
    b: Vec<f32>,
    ma: Vec<f32>,
    mb: Vec<f32>,
    tmp: Vec<f32>,
    sums: Vec<f64>,
    ra: Vec<f32>,
    rb: Vec<f32>,
    xs: Vec<(usize, usize, f32)>,
}

fn sized(v: &mut Vec<f32>, n: usize) {
    v.clear();
    v.resize(n, 0.0);
}

impl GuidedScratch {
    pub fn new() -> GuidedScratch {
        GuidedScratch::default()
    }

    /// Refine `mask` to `out_w` x `out_h` alpha (0..255) guided by the webcam luma,
    /// into `out` (resized to `out_w * out_h`).
    ///
    /// `radius` is the window radius in output pixels (applied at 1/4 size);
    /// `eps` regularises the guide variance (guide and mask in 0..1).
    /// The guide may have any size; it is scaled to the output size.
    /// Invalid inputs give a zeroed output; nothing indexes out of bounds.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &mut self,
        mask: &Mask,
        guide: &Frame,
        out_w: u32,
        out_h: u32,
        radius: u32,
        eps: f32,
        out: &mut Vec<u8>,
    ) {
        let (ow, oh) = (out_w as usize, out_h as usize);
        out.clear();
        out.resize(ow * oh, 0);
        if ow == 0
            || oh == 0
            || mask.width == 0
            || mask.height == 0
            || guide.width == 0
            || guide.height == 0
            || mask.data.len() != mask.width as usize * mask.height as usize
            || guide.data.len() != guide.width as usize * guide.height as usize * 4
        {
            return;
        }
        let s = SUBSAMPLE;
        let (lw, lh) = (ow.div_ceil(s), oh.div_ceil(s));
        let r = ((radius as usize) / s).max(1);
        let n = lw * lh;

        // Guide luma at output size.
        let rs = self.resizer.get_or_insert_with(Resizer::new);
        let px: &[u8] = if (guide.width, guide.height) == (out_w, out_h) {
            &guide.data
        } else {
            self.scaled.resize(ow * oh * 4, 0);
            resize_into(rs, guide, &mut self.scaled, out_w, out_h, false);
            &self.scaled
        };
        self.luma.clear();
        self.luma.extend(px.as_chunks::<4>().0.iter().map(luma8));

        // Low-resolution guide: mean luma of each s x s output block.
        sized(&mut self.i_low, n);
        for ly in 0..lh {
            let (y0, y1) = (ly * s, ((ly + 1) * s).min(oh));
            for lx in 0..lw {
                let (x0, x1) = (lx * s, ((lx + 1) * s).min(ow));
                let mut sum = 0u32;
                for y in y0..y1 {
                    sum += self.luma[y * ow + x0..y * ow + x1]
                        .iter()
                        .map(|&v| v as u32)
                        .sum::<u32>();
                }
                self.i_low[ly * lw + lx] = sum as f32 / (((y1 - y0) * (x1 - x0)) as f32 * 255.0);
            }
        }

        // Mask sampled at the centre of each block (same grid as the guide).
        sized(&mut self.p_low, n);
        let (mw, mh) = (mask.width as usize, mask.height as usize);
        let mtap = |lo: usize, hi: usize, on: usize, mn: usize| {
            let centre = (lo + hi) as f32 * 0.5;
            let c = (centre * mn as f32 / on as f32 - 0.5).clamp(0.0, (mn - 1) as f32);
            let i0 = c.floor() as usize;
            (i0, (i0 + 1).min(mn - 1), c - i0 as f32)
        };
        for ly in 0..lh {
            let (y0, y1, fy) = mtap(ly * s, ((ly + 1) * s).min(oh), oh, mh);
            for lx in 0..lw {
                let (x0, x1, fx) = mtap(lx * s, ((lx + 1) * s).min(ow), ow, mw);
                let m = |x: usize, y: usize| mask.data[y * mw + x] as f32;
                let top = m(x0, y0) + (m(x1, y0) - m(x0, y0)) * fx;
                let bot = m(x0, y1) + (m(x1, y1) - m(x0, y1)) * fx;
                self.p_low[ly * lw + lx] = (top + (bot - top) * fy) * (1.0 / 255.0);
            }
        }

        for v in [
            &mut self.ip,
            &mut self.ii,
            &mut self.mean_i,
            &mut self.mean_p,
            &mut self.corr_ip,
            &mut self.corr_ii,
            &mut self.a,
            &mut self.b,
            &mut self.ma,
            &mut self.mb,
            &mut self.tmp,
        ] {
            sized(v, n);
        }
        for k in 0..n {
            self.ip[k] = self.i_low[k] * self.p_low[k];
            self.ii[k] = self.i_low[k] * self.i_low[k];
        }
        let (tmp, sums) = (&mut self.tmp, &mut self.sums);
        box_mean(&self.i_low, lw, lh, r, &mut self.mean_i, tmp, sums);
        box_mean(&self.p_low, lw, lh, r, &mut self.mean_p, tmp, sums);
        box_mean(&self.ip, lw, lh, r, &mut self.corr_ip, tmp, sums);
        box_mean(&self.ii, lw, lh, r, &mut self.corr_ii, tmp, sums);
        for k in 0..n {
            let var = (self.corr_ii[k] - self.mean_i[k] * self.mean_i[k]).max(0.0);
            let cov = self.corr_ip[k] - self.mean_i[k] * self.mean_p[k];
            self.a[k] = cov / (var + eps);
            self.b[k] = self.mean_p[k] - self.a[k] * self.mean_i[k];
        }
        box_mean(&self.a, lw, lh, r, &mut self.ma, tmp, sums);
        box_mean(&self.b, lw, lh, r, &mut self.mb, tmp, sums);

        // Final pass: upsample mean_a / mean_b bilinearly and apply q = a*I + b.
        // Rows are blended vertically once, then each pixel needs one horizontal lerp each.
        self.xs.clear();
        self.xs.extend((0..ow).map(|x| taps_cells(x, s, lw)));
        sized(&mut self.ra, lw);
        sized(&mut self.rb, lw);
        for y in 0..oh {
            let (y0, y1, fy) = taps_cells(y, s, lh);
            for x in 0..lw {
                let (a0, a1) = (self.ma[y0 * lw + x], self.ma[y1 * lw + x]);
                let (b0, b1) = (self.mb[y0 * lw + x], self.mb[y1 * lw + x]);
                self.ra[x] = a0 + (a1 - a0) * fy;
                self.rb[x] = b0 + (b1 - b0) * fy;
            }
            let src = &self.luma[y * ow..(y + 1) * ow];
            let drow = &mut out[y * ow..(y + 1) * ow];
            let (ra, rb) = (&self.ra, &self.rb);
            for ((o, &l), &(x0, x1, fx)) in drow.iter_mut().zip(src).zip(&self.xs) {
                let a = ra[x0] + (ra[x1] - ra[x0]) * fx;
                let b = rb[x0] + (rb[x1] - rb[x0]) * fx;
                let q = a * (l as f32 * (1.0 / 255.0)) + b;
                *o = (q * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// Convenience wrapper around [`GuidedScratch::run`] that allocates fresh buffers.
pub fn guided_upsample(
    mask: &Mask,
    guide: &Frame,
    out_w: u32,
    out_h: u32,
    radius: u32,
    eps: f32,
) -> Vec<u8> {
    let mut out = Vec::new();
    GuidedScratch::new().run(mask, guide, out_w, out_h, radius, eps, &mut out);
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

    /// 10-90% transition width and the position where the output crosses 50%.
    fn transition(row: &[u8]) -> (usize, usize) {
        let lo = row.iter().position(|&v| v >= 26).unwrap();
        let hi = row.iter().position(|&v| v >= 230).unwrap();
        let mid = row.iter().position(|&v| v >= 128).unwrap();
        (hi - lo, mid)
    }

    #[test]
    fn guided_keeps_hard_edge_sharp() {
        // (width, true edge column); 637 is not a multiple of 4.
        for (w, edge) in [(640u32, 320u32), (637, 318), (641, 321)] {
            let h = 360u32;
            let guide = step_guide(w, h, edge);
            let mask = blurry_mask(160, 90, edge as f32 / w as f32, 0.03);
            let out = guided_upsample(&mask, &guide, w, h, R, EPS);
            let row = &out[(h as usize / 2) * w as usize..][..w as usize];
            let (width, mid) = transition(row);
            assert!(width <= 3, "w={w}: 10-90% transition {width} px");
            assert!(
                mid.abs_diff(edge as usize) <= 2,
                "w={w}: edge at {mid}, expected {edge}"
            );
            assert!(row[10] < 10 && row[w as usize - 10] > 245);
        }
    }

    /// Textured guide: diagonal gradient plus a checker.
    fn varied_guide(w: u32, h: u32) -> Frame {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = ((x * 255 / w + y * 255 / h) / 2) as u8 / 2
                    + if (x / 5 + y / 7) % 2 == 0 { 60 } else { 0 };
                data.extend_from_slice(&[v, v / 2 + 20, 255 - v, 255]);
            }
        }
        Frame::from_bgra(w, h, data).unwrap()
    }

    #[test]
    fn guided_varied_guide_odd_size_keeps_values() {
        let guide = varied_guide(333, 201);
        let (ow, oh) = (257u32, 143u32);
        for (v, tol) in [(255u8, 2u8), (0, 2)] {
            let mask = Mask::new(64, 36, vec![v; 64 * 36]).unwrap();
            let out = guided_upsample(&mask, &guide, ow, oh, R, EPS);
            assert!(out.iter().all(|&o| o.abs_diff(v) <= tol), "flat {v}");
        }
        // Left half person, right half room; far from the seam the values hold
        // whatever the guide texture does.
        let data: Vec<u8> = (0..64 * 36)
            .map(|i| if (i % 64) < 32 { 255 } else { 0 })
            .collect();
        let mask = Mask::new(64, 36, data).unwrap();
        let out = guided_upsample(&mask, &guide, ow, oh, R, EPS);
        for y in 0..oh as usize {
            let row = &out[y * ow as usize..][..ow as usize];
            assert!(row[..40].iter().all(|&v| v >= 230), "left y={y}");
            assert!(
                row[ow as usize - 40..].iter().all(|&v| v <= 25),
                "right y={y}"
            );
        }
    }

    #[test]
    fn scratch_matches_free_function_and_is_reusable() {
        let guide = varied_guide(320, 180);
        let mask = blurry_mask(64, 36, 0.5, 0.05);
        let want = guided_upsample(&mask, &guide, 200, 100, R, EPS);
        let mut s = GuidedScratch::new();
        let mut out = Vec::new();
        for _ in 0..3 {
            s.run(&mask, &guide, 200, 100, R, EPS, &mut out);
            assert_eq!(out, want);
        }
        // Size change is fine.
        s.run(&mask, &guide, 33, 17, R, EPS, &mut out);
        assert_eq!(out.len(), 33 * 17);
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

    fn time_ms(mask: &Mask, guide: &Frame, ow: u32, oh: u32, scratch: &mut GuidedScratch) -> f64 {
        let mut out = Vec::new();
        scratch.run(mask, guide, ow, oh, R, EPS, &mut out);
        let n = 30;
        let t = std::time::Instant::now();
        for _ in 0..n {
            scratch.run(mask, guide, ow, oh, R, EPS, &mut out);
            std::hint::black_box(&out);
        }
        t.elapsed().as_secs_f64() * 1000.0 / n as f64
    }

    #[test]
    #[ignore = "speed measurement; run with --release --nocapture"]
    fn guided_speed_1280x720() {
        // Real case: 1280x720 webcam guide, 960x540 PIP-sized output, single-threaded.
        let guide = varied_guide(1280, 720);
        let mut scratch = GuidedScratch::new();
        for (mw, mh) in [(256, 144), (640, 360)] {
            let mask = blurry_mask(mw, mh, 0.47, 0.05);
            let ms = time_ms(&mask, &guide, 960, 540, &mut scratch);
            println!("guided 1280x720 guide, {mw}x{mh} mask -> 960x540: {ms:.2} ms (budget 6)");
        }
        let mask = blurry_mask(256, 144, 0.47, 0.05);
        let ms = time_ms(&mask, &guide, 1280, 720, &mut scratch);
        println!("guided 1280x720 guide, 256x144 mask -> 1280x720: {ms:.2} ms");
    }
}
