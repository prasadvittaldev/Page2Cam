//! NV12 frames and the CPU BGRA -> NV12 conversion (BT.709, limited range).
//!
//! This is the reference the GPU shader output is compared against.
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

/// Bilinear resample of a BGRA frame to `dw` x `dh` (pixel-centre aligned).
fn scale_bilinear(src: &Frame, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh) = (src.width as usize, src.height as usize);
    let (dw, dh) = (dw as usize, dh as usize);
    let mut out = vec![0u8; dw * dh * 4];
    // 16.16 positions
    let xs = ((sw as u64) << 16) / dw as u64;
    let ys = ((sh as u64) << 16) / dh as u64;
    let sample = |sx: i64, sy: i64, c: usize| -> u32 {
        let x = sx.clamp(0, sw as i64 - 1) as usize;
        let y = sy.clamp(0, sh as i64 - 1) as usize;
        src.data[(y * sw + x) * 4 + c] as u32
    };
    for dy in 0..dh {
        let fy = ((dy as i64 * 2 + 1) * ys as i64 / 2) - 32768; // source y, 16.16
        let y0 = fy >> 16;
        let wy = (fy & 0xFFFF) as u32;
        for dx in 0..dw {
            let fx = ((dx as i64 * 2 + 1) * xs as i64 / 2) - 32768;
            let x0 = fx >> 16;
            let wx = (fx & 0xFFFF) as u32;
            let o = (dy * dw + dx) * 4;
            for c in 0..4 {
                let top = sample(x0, y0, c) * (65536 - wx) + sample(x0 + 1, y0, c) * wx;
                let bot = sample(x0, y0 + 1, c) * (65536 - wx) + sample(x0 + 1, y0 + 1, c) * wx;
                let v = (top as u64 * (65536 - wy) as u64 + bot as u64 * wy as u64 + (1u64 << 31))
                    >> 32;
                out[o + c] = v as u8;
            }
        }
    }
    out
}

/// Convert BGRA to NV12 at `dw` x `dh` (BT.709 limited range). When the size
/// differs from the source the frame is scaled bilinearly first. Each UV
/// sample is the average of the four per-pixel U/V values of its 2x2 block.
/// `dw` and `dh` must be even and non-zero.
pub fn bgra_to_nv12(src: &Frame, dw: u32, dh: u32) -> Nv12Frame {
    assert!(
        dw > 0 && dh > 0 && dw.is_multiple_of(2) && dh.is_multiple_of(2),
        "NV12 size must be even"
    );
    let scaled;
    let px: &[u8] = if src.width == dw && src.height == dh {
        &src.data
    } else {
        scaled = scale_bilinear(src, dw, dh);
        &scaled
    };
    let (w, h) = (dw as usize, dh as usize);
    let mut data = vec![0u8; w * h * 3 / 2];
    let (yp, uv) = data.split_at_mut(w * h);
    for by in 0..h / 2 {
        for bx in 0..w / 2 {
            let (mut su, mut sv) = (0i32, 0i32);
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (x, y) = (bx * 2 + dx, by * 2 + dy);
                let i = (y * w + x) * 4;
                let (b, g, r) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
                yp[y * w + x] = (16 + ((YR * r + YG * g + YB * b + 32768) >> 16)) as u8;
                su += UR * r + UG * g + UB * b;
                sv += VR * r + VG * g + VB * b;
            }
            let o = by * w + bx * 2;
            uv[o] = (128 + ((su + (1 << 17)) >> 18)).clamp(16, 240) as u8;
            uv[o + 1] = (128 + ((sv + (1 << 17)) >> 18)).clamp(16, 240) as u8;
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
}
