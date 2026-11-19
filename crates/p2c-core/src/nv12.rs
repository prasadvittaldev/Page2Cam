//! NV12 frames and the CPU BGRA -> NV12 conversion (BT.709, limited range).
//!
//! This is the reference the GPU shader output is compared against.
#![allow(clippy::chunks_exact_to_as_chunks)]
use crate::{Frame, FrameError};

/// A tightly packed NV12 frame: a Y plane (`width * height`) followed by an
/// interleaved UV plane (`width * height / 2`). Width and height are even.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nv12Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Nv12Frame {
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Result<Nv12Frame, FrameError> {
        let expected = width as usize * height as usize * 3 / 2;
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) || data.len() != expected {
            return Err(FrameError::BadLength {
                width,
                height,
                expected,
                got: data.len(),
            });
        }
        Ok(Nv12Frame {
            width,
            height,
            data,
        })
    }
}

/// An NV12 camera frame and the `seq` its BGRA twin got in the webcam
/// `FrameSlot`, so a consumer can tell whether the two belong together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nv12Capture {
    pub bgra_seq: u64,
    pub frame: Nv12Frame,
}

/// Shared "the cut-out wants NV12" switch. The segmenter turns it on while it
/// is enabled; capture only copies/publishes NV12 frames while it is on.
#[derive(Clone, Debug, Default)]
pub struct Nv12Gate(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Nv12Gate {
    pub fn new() -> Nv12Gate {
        Nv12Gate::default()
    }

    pub fn set(&self, on: bool) {
        self.0.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn is_on(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Cut `crop` out of an NV12 frame with the same rectangle `apply_crop` uses
/// for BGRA (`Crop::pixels`: all values even, so chroma pairs stay whole).
pub fn crop_nv12(frame: Nv12Frame, crop: &crate::Crop) -> Nv12Frame {
    let (x, y, w, h) = crop.pixels(frame.width, frame.height);
    if (x, y, w, h) == (0, 0, frame.width, frame.height) {
        return frame;
    }
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    let (x, y, w, h) = (x as usize, y as usize, w as usize, h as usize);
    let mut data = Vec::with_capacity(w * h * 3 / 2);
    for r in y..y + h {
        data.extend_from_slice(&frame.data[r * fw + x..r * fw + x + w]);
    }
    let uv = &frame.data[fw * fh..];
    for r in y / 2..(y + h) / 2 {
        data.extend_from_slice(&uv[r * fw + x..r * fw + x + w]);
    }
    Nv12Frame {
        width: w as u32,
        height: h as u32,
        data,
    }
}

// BT.709 limited range, 16.16 fixed point. Chroma coefficients sum to exactly 0.
const YR: i32 = 11966;
const YG: i32 = 40255;
const YB: i32 = 4064;
const UR: i32 = -6596;
const UG: i32 = -22188;
const UB: i32 = 28784;
const VR: i32 = 28784;
const VG: i32 = -26145;
const VB: i32 = -2639;

/// Fit `(w, h)` into 1920x1080 keeping the aspect ratio, then floor each
/// dimension to even. Sizes that already fit are only forced even.
pub fn camera_size(w: u32, h: u32) -> (u32, u32) {
    let (mut ow, mut oh) = (w as u64, h as u64);
    if w > 1920 || h > 1080 {
        // scale = min(1920/w, 1080/h), done in integers
        if 1920 * h as u64 <= 1080 * w as u64 {
            ow = 1920;
            oh = 1920 * h as u64 / w as u64;
        } else {
            oh = 1080;
            ow = 1080 * w as u64 / h as u64;
        }
    }
    ((ow as u32) & !1, (oh as u32) & !1)
}

/// Convert one pair of BGRA rows (width `w`, even) into two Y rows and one UV row.
fn convert_row_pair(r0: &[u8], r1: &[u8], y0: &mut [u8], y1: &mut [u8], uv: &mut [u8]) {
    let blocks = r0
        .chunks_exact(8)
        .zip(r1.chunks_exact(8))
        .zip(y0.chunks_exact_mut(2).zip(y1.chunks_exact_mut(2)))
        .zip(uv.chunks_exact_mut(2));
    for (((a, b), (ya, yb)), uvo) in blocks {
        let (mut su, mut sv) = (0i32, 0i32);
        for (px, y) in [
            (&a[0..4], 0usize),
            (&a[4..8], 1),
            (&b[0..4], 2),
            (&b[4..8], 3),
        ] {
            let (b, g, r) = (px[0] as i32, px[1] as i32, px[2] as i32);
            let yv = (16 + ((YR * r + YG * g + YB * b + 32768) >> 16)) as u8;
            if y < 2 {
                ya[y] = yv;
            } else {
                yb[y - 2] = yv;
            }
            su += UR * r + UG * g + UB * b;
            sv += VR * r + VG * g + VB * b;
        }
        uvo[0] = (128 + ((su + (1 << 17)) >> 18)).clamp(16, 240) as u8;
        uvo[1] = (128 + ((sv + (1 << 17)) >> 18)).clamp(16, 240) as u8;
    }
}

/// Horizontal taps for bilinear scaling: (x0 byte offset, x1 byte offset, weight of x1 in 0..=256).
fn x_taps(sw: usize, dw: usize) -> Vec<(usize, usize, u32)> {
    let xs = ((sw as u64) << 16) / dw as u64;
    (0..dw)
        .map(|dx| {
            let fx = ((dx as i64 * 2 + 1) * xs as i64 / 2) - 32768; // 16.16 source x
            let x0 = fx >> 16;
            let w = (((fx & 0xFFFF) + 128) >> 8) as u32;
            let c = |x: i64| x.clamp(0, sw as i64 - 1) as usize * 4;
            (c(x0), c(x0 + 1), w)
        })
        .collect()
}

/// Bilinear-scale destination row `dy` of the source into `out` (BGRA, `taps.len()` pixels).
fn scale_row(src: &Frame, dy: usize, dh: usize, taps: &[(usize, usize, u32)], out: &mut [u8]) {
    let (sw, sh) = (src.width as usize, src.height as usize);
    let ys = ((sh as u64) << 16) / dh as u64;
    let fy = ((dy as i64 * 2 + 1) * ys as i64 / 2) - 32768;
    let wy = (((fy & 0xFFFF) + 128) >> 8) as u32;
    let row = |y: i64| {
        let y = y.clamp(0, sh as i64 - 1) as usize;
        &src.data[y * sw * 4..(y + 1) * sw * 4]
    };
    let (ra, rb) = (row(fy >> 16), row((fy >> 16) + 1));
    for (o, &(x0, x1, wx)) in out.chunks_exact_mut(4).zip(taps) {
        for c in 0..4 {
            let top = ra[x0 + c] as u32 * (256 - wx) + ra[x1 + c] as u32 * wx;
            let bot = rb[x0 + c] as u32 * (256 - wx) + rb[x1 + c] as u32 * wx;
            o[c] = ((top * (256 - wy) + bot * wy + 32768) >> 16) as u8;
        }
    }
}

/// Convert BGRA to NV12 at `dw` x `dh` (BT.709 limited range). When the size
/// differs from the source the frame is scaled bilinearly (row by row, no
/// full-size intermediate). Each UV sample is the average of the four
/// per-pixel U/V values of its 2x2 block. `dw` and `dh` must be even and
/// non-zero.
pub fn bgra_to_nv12(src: &Frame, dw: u32, dh: u32) -> Nv12Frame {
    assert!(
        dw > 0 && dh > 0 && dw.is_multiple_of(2) && dh.is_multiple_of(2),
        "NV12 size must be even"
    );
    let (w, h) = (dw as usize, dh as usize);
    let mut data = vec![0u8; w * h * 3 / 2];
    let (yp, uv) = data.split_at_mut(w * h);
    let ypairs = yp.chunks_exact_mut(w * 2).zip(uv.chunks_exact_mut(w));
    if src.width == dw && src.height == dh {
        let rows = src.data.chunks_exact(w * 8);
        for ((yrows, uvrow), pair) in ypairs.zip(rows) {
            let (r0, r1) = pair.split_at(w * 4);
            let (y0, y1) = yrows.split_at_mut(w);
            convert_row_pair(r0, r1, y0, y1, uvrow);
        }
    } else {
        let taps = x_taps(src.width as usize, w);
        let mut rows = vec![0u8; w * 8];
        for (by, (yrows, uvrow)) in ypairs.enumerate() {
            let (r0, r1) = rows.split_at_mut(w * 4);
            scale_row(src, by * 2, h, &taps, r0);
            scale_row(src, by * 2 + 1, h, &taps, r1);
            let (y0, y1) = yrows.split_at_mut(w);
            convert_row_pair(r0, r1, y0, y1, uvrow);
        }
    }
    Nv12Frame {
        width: dw,
        height: dh,
        data,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, bgra: [u8; 4]) -> Frame {
        Frame::from_bgra(w, h, bgra.repeat((w * h) as usize)).unwrap()
    }
    fn near(a: u8, b: u8) -> bool {
        (a as i32 - b as i32).abs() <= 1
    }

    #[test]
    fn white_black_red_bt709() {
        let n = bgra_to_nv12(&solid(4, 4, [255, 255, 255, 255]), 4, 4);
        assert!(near(n.data[0], 235) && near(n.data[16], 128) && near(n.data[17], 128));
        let n = bgra_to_nv12(&solid(4, 4, [0, 0, 0, 255]), 4, 4);
        assert!(near(n.data[0], 16) && near(n.data[16], 128) && near(n.data[17], 128));
        let n = bgra_to_nv12(&solid(4, 4, [0, 0, 255, 255]), 4, 4);
        assert!(near(n.data[0], 63), "Y {}", n.data[0]);
        assert!(near(n.data[16], 102), "U {}", n.data[16]);
        assert!(near(n.data[17], 240), "V {}", n.data[17]);
    }

    #[test]
    fn uv_is_block_average() {
        let red = bgra_to_nv12(&solid(2, 2, [0, 0, 255, 255]), 2, 2);
        let blue = bgra_to_nv12(&solid(2, 2, [255, 0, 0, 255]), 2, 2);
        let f = Frame::from_bgra(
            2,
            2,
            vec![
                0, 0, 255, 255, 255, 0, 0, 255, 0, 0, 255, 255, 255, 0, 0, 255,
            ],
        )
        .unwrap();
        let n = bgra_to_nv12(&f, 2, 2);
        let (u, v) = (n.data[4], n.data[5]);
        assert!(
            near(u, ((red.data[4] as u16 + blue.data[4] as u16) / 2) as u8),
            "U {u}"
        );
        assert!(
            near(v, ((red.data[5] as u16 + blue.data[5] as u16) / 2) as u8),
            "V {v}"
        );
    }

    #[test]
    fn camera_size_fits_1080p() {
        assert_eq!(camera_size(1920, 1080), (1920, 1080));
        assert_eq!(camera_size(1280, 720), (1280, 720));
        assert_eq!(camera_size(3840, 2160), (1920, 1080));
        assert_eq!(camera_size(2560, 1080), (1920, 810));
        assert_eq!(camera_size(1080, 1920), (606, 1080));
    }

    #[test]
    fn nv12_new_checks_length() {
        assert!(Nv12Frame::new(4, 2, vec![0; 12]).is_ok());
        assert!(Nv12Frame::new(4, 2, vec![0; 11]).is_err());
        assert!(Nv12Frame::new(3, 2, vec![0; 9]).is_err());
    }

    #[test]
    fn scaling_keeps_solid_colour_and_is_fast_enough() {
        let n = bgra_to_nv12(&solid(64, 48, [0, 0, 255, 255]), 32, 24);
        assert!(near(n.data[0], 63) && near(n.data[32 * 24], 102));
    }

    #[test]
    #[ignore]
    fn bench_bgra_to_nv12_1080p() {
        fn median_ms(src: &Frame, dw: u32, dh: u32) -> f64 {
            let mut v: Vec<f64> = (0..50)
                .map(|_| {
                    let t = std::time::Instant::now();
                    std::hint::black_box(bgra_to_nv12(std::hint::black_box(src), dw, dh));
                    t.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        }
        let mk = |w: u32, h: u32| {
            let data = (0..w as usize * h as usize * 4)
                .map(|i| (i * 7 % 251) as u8)
                .collect();
            Frame::from_bgra(w, h, data).unwrap()
        };
        println!(
            "1920x1080 same size: {:.2} ms",
            median_ms(&mk(1920, 1080), 1920, 1080)
        );
        println!(
            "3840x2160 -> 1920x1080: {:.2} ms",
            median_ms(&mk(3840, 2160), 1920, 1080)
        );
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use crate::Crop;

    #[test]
    fn full_crop_moves_the_nv12_buffer() {
        let f = Nv12Frame::new(4, 4, vec![7; 24]).unwrap();
        let p = f.data.as_ptr();
        assert_eq!(crop_nv12(f, &Crop::FULL).data.as_ptr(), p);
    }

    #[test]
    fn gate_is_shared_and_starts_off() {
        let g = Nv12Gate::new();
        let g2 = g.clone();
        assert!(!g.is_on());
        g2.set(true);
        assert!(g.is_on());
    }
}
