//! Webcam crop: a normalized rectangle applied to each camera frame.
use std::sync::Arc;

use crate::Frame;

/// Smallest crop side as a fraction of the camera frame.
const MIN_SIDE: f32 = 0.10;

/// A crop rectangle in 0..1 camera coordinates (top-left origin).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crop {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The crop shared between the admin page and the capture thread.
pub type SharedCrop = Arc<parking_lot::RwLock<Crop>>;

impl Crop {
    pub const FULL: Crop = Crop {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    pub fn is_full(&self) -> bool {
        *self == Crop::FULL
    }

    /// Force a valid rectangle: NaN anywhere gives FULL, `w`/`h` are at least
    /// 0.10, and `x`/`y` keep the rectangle inside 0..1.
    pub fn clamp(self) -> Crop {
        if [self.x, self.y, self.w, self.h].iter().any(|v| v.is_nan()) {
            return Crop::FULL;
        }
        let w = self.w.clamp(MIN_SIDE, 1.0);
        let h = self.h.clamp(MIN_SIDE, 1.0);
        Crop {
            x: self.x.clamp(0.0, 1.0 - w),
            y: self.y.clamp(0.0, 1.0 - h),
            w,
            h,
        }
    }

    /// The crop in pixels of a `cam_w` x `cam_h` frame as (x, y, w, h): all
    /// even (NV12 needs it), w and h at least 2, inside the frame.
    pub fn pixels(&self, cam_w: u32, cam_h: u32) -> (u32, u32, u32, u32) {
        let axis = |pos: f32, len: f32, total: u32| {
            let max = (total & !1).max(2);
            let l = ((len * total as f32).round() as u32 & !1).clamp(2, max);
            let p = ((pos * total as f32).round() as u32 & !1).min((max - l) & !1);
            (p, l)
        };
        let (x, w) = axis(self.x, self.w, cam_w);
        let (y, h) = axis(self.y, self.h, cam_h);
        (x, y, w, h)
    }
}

/// Cut `crop` out of `frame`. A full crop returns `frame` itself (no copy).
pub fn apply_crop(frame: Frame, crop: &Crop) -> Frame {
    if crop.is_full() {
        return frame;
    }
    let (x, y, w, h) = crop.pixels(frame.width, frame.height);
    if (x, y, w, h) == (0, 0, frame.width, frame.height) {
        return frame;
    }
    let stride = frame.width as usize * 4;
    let row = w as usize * 4;
    let mut data = Vec::with_capacity(row * h as usize);
    for r in y as usize..(y + h) as usize {
        let start = r * stride + x as usize * 4;
        data.extend_from_slice(&frame.data[start..start + row]);
    }
    Frame {
        width: w,
        height: h,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_crop_is_identity_without_copy() {
        let f = Frame::new_black(4, 4);
        let p = f.data.as_ptr();
        let out = apply_crop(f, &Crop::FULL);
        assert_eq!(out.data.as_ptr(), p);
    }

    #[test]
    fn crop_copies_the_region() {
        let mut data = Vec::new();
        for i in 0..16u8 {
            data.extend_from_slice(&[i, i, i, 255]);
        }
        let f = Frame::from_bgra(4, 4, data).unwrap();
        let out = apply_crop(
            f,
            &Crop {
                x: 0.5,
                y: 0.0,
                w: 0.5,
                h: 0.5,
            },
        );
        assert_eq!((out.width, out.height), (2, 2));
        let got: Vec<u8> = out.data.chunks(4).map(|p| p[0]).collect();
        assert_eq!(got, vec![2, 3, 6, 7]);
    }

    #[test]
    fn pixels_even_inside_and_min_size() {
        let (x, y, w, h) = Crop {
            x: 0.95,
            y: 0.95,
            w: 0.10,
            h: 0.10,
        }
        .pixels(640, 480);
        assert!(x % 2 == 0 && y % 2 == 0 && w % 2 == 0 && h % 2 == 0);
        assert!(w >= 2 && h >= 2 && x + w <= 640 && y + h <= 480);
        let c = Crop {
            x: 0.25,
            y: 0.25,
            w: 0.5,
            h: 0.5,
        };
        assert_eq!(c.pixels(1280, 720), (320, 180, 640, 360));
    }

    #[test]
    fn clamp_rules() {
        let nan = Crop {
            x: f32::NAN,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        };
        assert_eq!(nan.clamp(), Crop::FULL);
        let c = Crop {
            x: 0.0,
            y: 0.0,
            w: 0.01,
            h: 1.0,
        }
        .clamp();
        assert_eq!(c.w, 0.10);
        let c = Crop {
            x: 0.95,
            y: 0.0,
            w: 0.5,
            h: 1.0,
        }
        .clamp();
        assert_eq!((c.x, c.w), (0.5, 0.5));
        assert!(Crop::FULL.is_full());
    }
}
