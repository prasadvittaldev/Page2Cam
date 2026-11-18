//! Turning a model's raw mask into the mask the compositor draws with.
use p2c_core::{EdgeRange, Mask};

use crate::EngineChoice;

/// How an engine's masks are cleaned up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Profile {
    /// Weight of the newest mask in the temporal smoothing (None = off).
    pub smoothing: Option<f32>,
    /// Edge tightening applied after the mask is scaled to the overlay.
    pub edge: EdgeRange,
}

/// Fast (MediaPipe) is a segmenter: its soft, flickery confidence map needs
/// smoothing over time and a narrow edge (≈ smoothstep 0.35..0.65). Quality
/// (RVM) is a real matting model with its own temporal memory: only trim the
/// faint haze and solidify the body, keep soft hair edges.
pub fn profile(choice: EngineChoice) -> Profile {
    match choice {
        EngineChoice::Fast => Profile {
            smoothing: Some(0.5),
            edge: EdgeRange { lo: 90, hi: 165 },
        },
        EngineChoice::Quality => Profile {
            smoothing: None,
            edge: EdgeRange { lo: 16, hi: 240 },
        },
    }
}

/// A change bigger than this at a pixel is real movement: follow it at once
/// instead of smoothing (no ghost trail behind a moving person).
const MOVE: f32 = 0.5;

/// Per-pixel exponential smoothing of successive masks.
pub struct Smoother {
    weight: f32,
    prev: Vec<f32>,
    size: (u32, u32),
}

impl Smoother {
    /// `weight` is how much of each new mask is taken (1.0 = no smoothing).
    pub fn new(weight: f32) -> Smoother {
        Smoother {
            weight: weight.clamp(0.0, 1.0),
            prev: Vec::new(),
            size: (0, 0),
        }
    }

    pub fn reset(&mut self) {
        self.prev.clear();
    }

    pub fn apply(&mut self, mask: &[f32], w: u32, h: u32) -> Vec<f32> {
        if self.size != (w, h) || self.prev.len() != mask.len() {
            self.size = (w, h);
            self.prev = mask.to_vec();
            return self.prev.clone();
        }
        for (p, &n) in self.prev.iter_mut().zip(mask) {
            *p = if (n - *p).abs() > MOVE {
                n
            } else {
                *p + (n - *p) * self.weight
            };
        }
        self.prev.clone()
    }
}

/// A 0..1 float mask as a one-byte [`Mask`] carrying `edge`. Errors if
/// `values` doesn't hold `w * h` entries.
pub fn to_mask(values: &[f32], w: u32, h: u32, edge: EdgeRange) -> anyhow::Result<Mask> {
    let data = values
        .iter()
        .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();
    let mut m = Mask::new(w, h, data)?;
    m.edge = edge;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2c_core::EdgeRange;

    #[test]
    fn first_frame_passes_through() {
        let mut s = Smoother::new(0.5);
        assert_eq!(s.apply(&[0.2, 0.9], 2, 1), vec![0.2, 0.9]);
    }

    #[test]
    fn small_changes_are_smoothed() {
        let mut s = Smoother::new(0.5);
        s.apply(&[0.4], 1, 1);
        let v = s.apply(&[0.6], 1, 1)[0];
        assert!((v - 0.5).abs() < 1e-6, "{v}");
    }

    #[test]
    fn big_changes_are_followed_at_once() {
        // Someone moving: no ghost trail behind them.
        let mut s = Smoother::new(0.5);
        s.apply(&[0.0, 1.0], 2, 1);
        assert_eq!(s.apply(&[1.0, 0.0], 2, 1), vec![1.0, 0.0]);
    }

    #[test]
    fn size_change_starts_afresh() {
        let mut s = Smoother::new(0.5);
        s.apply(&[0.0], 1, 1);
        assert_eq!(s.apply(&[0.3, 0.3], 2, 1), vec![0.3, 0.3]);
    }

    #[test]
    fn reset_forgets_the_last_mask() {
        let mut s = Smoother::new(0.5);
        s.apply(&[0.4], 1, 1);
        s.reset();
        assert_eq!(s.apply(&[0.6], 1, 1), vec![0.6]);
    }

    #[test]
    fn to_mask_clamps_rounds_and_keeps_edge() {
        let edge = EdgeRange { lo: 10, hi: 200 };
        let m = to_mask(&[-0.5, 0.5, 1.7], 3, 1, edge).unwrap();
        assert_eq!(m.data, vec![0, 128, 255]);
        assert_eq!((m.width, m.height, m.edge), (3, 1, edge));
    }

    #[test]
    fn to_mask_rejects_wrong_size() {
        assert!(to_mask(&[0.0; 3], 2, 2, EdgeRange::FULL).is_err());
    }
}
