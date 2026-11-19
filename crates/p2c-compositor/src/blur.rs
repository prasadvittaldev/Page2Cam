//! Cached blur of the webcam picture (room background behind a cut-out).
use crate::compose::{resize_cover_into, resize_into};
use fast_image_resize::Resizer;
use p2c_core::Frame;
use std::sync::Arc;

/// Blur works at no more than 1/4 of the output size, and never wider than this.
/// The blur is very smooth, so a larger output only needs a larger upscale, not
/// a bigger kernel: the cost stays flat from 960x540 up to 1920x1080.
const MAX_LOW_W: u32 = 160;
/// Gaussian sigma as a fraction of the output width.
const SIGMA_FRAC: f32 = 0.02;

/// A blurred copy of the webcam frame at output size, rebuilt only when the
/// source frame (by identity) or the requested size changes. Buffers and the
/// resizer are kept between rebuilds.
#[derive(Default)]
pub struct BlurCache {
    src: Option<Arc<Frame>>,
    size: (u32, u32),
    /// Built cover-scaled ([`BlurCache::get_cover`]) rather than stretched.
    cover: bool,
    out: Vec<u8>,
    builds: u32,
    resizer: Option<Resizer>,
    small: Vec<u8>,
    blurred: Vec<u8>,
    tmp: Vec<f32>,
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
        self.get_fitted(cam, w, h, false)
    }

    /// Like [`BlurCache::get`], but the webcam is cover-scaled: its centre is
    /// cropped to the `w` x `h` aspect ratio while scaling down, so nothing is
    /// squeezed and the blur is still built at the requested size.
    pub fn get_cover(&mut self, cam: &Arc<Frame>, w: u32, h: u32) -> &[u8] {
        self.get_fitted(cam, w, h, true)
    }

    fn get_fitted(&mut self, cam: &Arc<Frame>, w: u32, h: u32, cover: bool) -> &[u8] {
        let fresh = self.src.as_ref().is_some_and(|s| Arc::ptr_eq(s, cam))
            && self.size == (w, h)
            && self.cover == cover;
        if !fresh {
            self.rebuild(cam, w, h, cover);
            self.src = Some(Arc::clone(cam));
            self.size = (w, h);
            self.cover = cover;
            self.builds += 1;
        }
        &self.out
    }

    fn rebuild(&mut self, cam: &Frame, w: u32, h: u32, cover: bool) {
        self.out.clear();
        if w == 0 || h == 0 {
            return;
        }
        let n = w as usize * h as usize;
        if cam.width == 0
            || cam.height == 0
            || cam.data.len() != cam.width as usize * cam.height as usize * 4
        {
            self.out.resize(n * 4, 0);
            self.out.as_chunks_mut::<4>().0.fill([0, 0, 0, 255]);
            return;
        }
        let lw = (w / 4).clamp(1, MAX_LOW_W);
        let lh = ((h as u64 * lw as u64 / w as u64) as u32).max(1);
        let (lwu, lhu) = (lw as usize, lh as usize);
        let resizer = self.resizer.get_or_insert_with(Resizer::new);
        self.small.resize(lwu * lhu * 4, 0);
        if cover {
            resize_cover_into(resizer, cam, &mut self.small, lw, lh);
        } else {
            resize_into(resizer, cam, &mut self.small, lw, lh, false);
        }
        // Sigma in low-resolution pixels.
        let sigma = (SIGMA_FRAC * lw as f32).max(1.0);
        gaussian(
            &self.small,
            &mut self.blurred,
            &mut self.tmp,
            lwu,
            lhu,
            sigma,
        );
        self.out.resize(n * 4, 0);
        let blurred = Frame {
            width: lw,
            height: lh,
            data: std::mem::take(&mut self.blurred),
        };
        resize_into(resizer, &blurred, &mut self.out, w, h, false);
        self.blurred = blurred.data;
        for px in self.out.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
    }
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

/// Separable Gaussian on the B, G, R channels (edges clamped); `dst` alpha opaque.
fn gaussian(src: &[u8], dst: &mut Vec<u8>, tmp: &mut Vec<f32>, w: usize, h: usize, sigma: f32) {
    let k = kernel(sigma);
    let r = (k.len() / 2) as isize;
    tmp.clear();
    tmp.resize(w * h * 4, 0.0);
    dst.clear();
    dst.resize(w * h * 4, 255);
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 3];
            for (t, kv) in k.iter().enumerate() {
                let sx = (x as isize + t as isize - r).clamp(0, w as isize - 1) as usize;
                let i = (y * w + sx) * 4;
                for (c, a) in acc.iter_mut().enumerate() {
                    *a += kv * src[i + c] as f32;
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
                dst[o + c] = (a + 0.5) as u8;
            }
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

    #[test]
    #[ignore = "speed measurement; run with --release --nocapture"]
    fn blur_speed() {
        let mut d = Vec::new();
        for y in 0..720u32 {
            for x in 0..1280u32 {
                let v = ((x * 7 + y * 3) % 256) as u8;
                d.extend_from_slice(&[v, v / 2, 255 - v, 255]);
            }
        }
        let n = 20;
        for (w, h) in [(960u32, 540u32), (1920, 1080)] {
            let mut c = BlurCache::new();
            let mut frames: Vec<Arc<Frame>> = (0..n + 1)
                .map(|_| Arc::new(Frame::from_bgra(1280, 720, d.clone()).unwrap()))
                .collect();
            c.get(&frames[0], w, h); // warm-up, allocates buffers
            let t = std::time::Instant::now();
            for f in frames.drain(1..) {
                std::hint::black_box(c.get(&f, w, h));
            }
            println!(
                "blur 1280x720 -> {w}x{h}: {:.2} ms per new frame",
                t.elapsed().as_secs_f64() * 1000.0 / n as f64
            );
        }
    }
}
