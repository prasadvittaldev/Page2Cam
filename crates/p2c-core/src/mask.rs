use crate::FrameError;

/// How a mask's soft edge is tightened after it is scaled to its final size.
/// Values at or below `lo` become 0 (room), at or above `hi` 255 (person),
/// with a smooth ramp in between. Upscaling a small mask leaves a wide blurry
/// ramp; a narrower range makes the outline crisp again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeRange {
    pub lo: u8,
    pub hi: u8,
}

impl EdgeRange {
    /// Leave the mask as it is.
    pub const FULL: EdgeRange = EdgeRange { lo: 0, hi: 255 };

    /// Lookup table from a scaled mask value to the final alpha.
    pub fn lut(self) -> [u8; 256] {
        let mut t = [0u8; 256];
        if self == EdgeRange::FULL || self.hi <= self.lo {
            for (i, v) in t.iter_mut().enumerate() {
                *v = i as u8;
            }
            return t;
        }
        let (lo, hi) = (self.lo as f32, self.hi as f32);
        for (i, v) in t.iter_mut().enumerate() {
            let x = ((i as f32 - lo) / (hi - lo)).clamp(0.0, 1.0);
            *v = (x * x * (3.0 - 2.0 * x) * 255.0).round() as u8;
        }
        t
    }
}

/// A person mask, one byte per pixel (0 = room, 255 = person), row-major.
/// It covers the whole webcam picture at its own (usually lower) resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    /// Edge tightening to apply after scaling (depends on the model).
    pub edge: EdgeRange,
}

impl Mask {
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Result<Mask, FrameError> {
        let expected = width as usize * height as usize;
        if data.len() != expected {
            return Err(FrameError::BadLength {
                width,
                height,
                expected,
                got: data.len(),
            });
        }
        Ok(Mask {
            width,
            height,
            data,
            edge: EdgeRange::FULL,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_checks_length() {
        assert!(Mask::new(2, 2, vec![0; 3]).is_err());
        let m = Mask::new(2, 2, vec![0; 4]).unwrap();
        assert_eq!((m.width, m.height, m.edge), (2, 2, EdgeRange::FULL));
    }

    #[test]
    fn edge_range_maps_through_lut() {
        let lut = EdgeRange { lo: 64, hi: 192 }.lut();
        assert_eq!((lut[0], lut[64], lut[192], lut[255]), (0, 0, 255, 255));
        assert!((lut[128] as i32 - 128).abs() <= 1, "{}", lut[128]);
        // Smooth (no hard step) and monotonic inside the range.
        assert!(lut[100] > lut[80] && lut[100] < lut[128]);
        assert!(lut.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn full_range_is_identity() {
        let lut = EdgeRange::FULL.lut();
        assert!(lut.iter().enumerate().all(|(i, v)| *v as usize == i));
    }
}
