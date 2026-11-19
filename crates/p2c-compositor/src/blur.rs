//! Cached blur of the webcam picture (room background behind a cut-out).
use crate::scale_frame_straight;
use p2c_core::Frame;
use std::sync::Arc;

/// Blur works at no more than 1/4 of the output size.
const DOWN: u32 = 4;
/// Gaussian sigma as a fraction of the output width.
const SIGMA_FRAC: f32 = 0.02;

/// A blurred copy of the webcam frame at output size, rebuilt only when the
/// source frame (by identity) or the requested size changes.
#[derive(Default)]
pub struct BlurCache {
    src: Option<Arc<Frame>>,
    size: (u32, u32),
    out: Vec<u8>,
    builds: u32,
}

impl BlurCache {
    pub fn new() -> BlurCache {
        BlurCache::default()
    }

    /// How many times the blur has been computed (for tests).
    #[cfg(test)]
    fn builds(&self) -> u32 {
        self.builds
    }

    /// Opaque BGRA, `w * h * 4` bytes: the webcam stretched to `w` x `h` and blurred.
    pub fn get(&mut self, cam: &Arc<Frame>, w: u32, h: u32) -> &[u8] {
        let fresh = self.src.as_ref().is_some_and(|s| Arc::ptr_eq(s, cam)) && self.size == (w, h);
        if !fresh {
            self.out = blur_frame(cam, w, h);
            self.src = Some(Arc::clone(cam));
            self.size = (w, h);
            self.builds += 1;
        }
        &self.out
    }
}

fn blur_frame(cam: &Frame, w: u32, h: u32) -> Vec<u8> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    if cam.width == 0
        || cam.height == 0
        || cam.data.len() != cam.width as usize * cam.height as usize * 4
    {
        return [0u8, 0, 0, 255].repeat(w as usize * h as usize);
    }
    let (lw, lh) = ((w / DOWN).max(1), (h / DOWN).max(1));
    let mut small = scale_frame_straight(cam, lw, lh);
    // Sigma in low-resolution pixels.
    let sigma = (SIGMA_FRAC * lw as f32).max(1.0);
    gaussian(&mut small, sigma);
    let mut big = scale_frame_straight(&small, w, h);
    for px in big.data.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    big.data
}

fn kernel(sigma: f32) -> Vec<f32> {
    let r = (sigma * 3.0).ceil() as i32;
    let mut k: Vec<f32> = (-r..=r)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let s: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= s);
    k
}

/// Separable Gaussian on the B, G, R channels (edges clamped); alpha set opaque.
fn gaussian(f: &mut Frame, sigma: f32) {
    let (w, h) = (f.width as usize, f.height as usize);
    let k = kernel(sigma);
    let r = (k.len() / 2) as isize;
    let src: Vec<f32> = f.data.iter().map(|&v| v as f32).collect();
    let mut tmp = vec![0.0f32; src.len()];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 3];
            for (t, kv) in k.iter().enumerate() {
                let sx = (x as isize + t as isize - r).clamp(0, w as isize - 1) as usize;
                let i = (y * w + sx) * 4;
                for (c, a) in acc.iter_mut().enumerate() {
                    *a += kv * src[i + c];
                }
            }
            tmp[(y * w + x) * 4..][..3].copy_from_slice(&acc);
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 3];
            for (t, kv) in k.iter().enumerate() {
                let sy = (y as isize + t as isize - r).clamp(0, h as isize - 1) as usize;
                let i = (sy * w + x) * 4;
                for (c, a) in acc.iter_mut().enumerate() {
                    *a += kv * tmp[i + c];
                }
            }
            let o = (y * w + x) * 4;
            for (c, a) in acc.iter().enumerate() {
                f.data[o + c] = (a + 0.5) as u8;
            }
            f.data[o + 3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(w: u32, h: u32) -> Arc<Frame> {
        let mut d = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if (x / 4 + y / 4) % 2 == 0 { 0 } else { 255 };
                d.extend_from_slice(&[v, v, v, 255]);
            }
        }
        Arc::new(Frame::from_bgra(w, h, d).unwrap())
    }

    #[test]
    fn blur_is_smooth_and_same_size() {
        let cam = checker(320, 180);
        let mut c = BlurCache::new();
        let out = c.get(&cam, 640, 360).to_vec();
        assert_eq!(out.len(), 640 * 360 * 4);
        assert!(out.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        // Adjacent pixels differ little, and the checker contrast is gone.
        let max_step = (0..out.len() / 4 - 1)
            .filter(|i| (i + 1) % 640 != 0)
            .map(|i| out[i * 4].abs_diff(out[(i + 1) * 4]))
            .max()
            .unwrap();
        assert!(max_step <= 12, "max step {max_step}");
        let (lo, hi) = out
            .as_chunks::<4>()
            .0
            .iter()
            .fold((255u8, 0u8), |(l, h), p| (l.min(p[0]), h.max(p[0])));
        assert!(hi - lo < 120, "range {lo}..{hi}");
    }

    #[test]
    fn blur_cache_rebuilds_on_size_change() {
        let cam = checker(64, 36);
        let mut c = BlurCache::new();
        assert_eq!(c.get(&cam, 128, 72).len(), 128 * 72 * 4);
        assert_eq!(c.get(&cam, 100, 50).len(), 100 * 50 * 4);
        assert_eq!(c.builds(), 2);
        // A different frame at the same size also rebuilds.
        let cam2 = checker(32, 18);
        assert_eq!(c.get(&cam2, 100, 50).len(), 100 * 50 * 4);
        assert_eq!(c.builds(), 3);
        assert!(c.get(&cam2, 0, 0).is_empty());
        assert_eq!(c.get(&cam2, 1, 1).len(), 4);
    }

    #[test]
    fn blur_cache_reuses_same_frame() {
        let cam = checker(64, 36);
        let mut c = BlurCache::new();
        c.get(&cam, 128, 72);
        c.get(&cam, 128, 72);
        assert_eq!(c.builds(), 1);
        let same_content = Arc::new((*cam).clone());
        c.get(&same_content, 128, 72);
        assert_eq!(c.builds(), 2);
    }
}
