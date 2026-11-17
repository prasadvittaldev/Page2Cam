//! Picture-in-picture placement.

/// Fraction of the output width/height kept clear around the PIP.
const MARGIN: f32 = 0.02;

/// Where and how big the webcam overlay is.
///
/// `x`/`y` place the PIP inside the area left after a 2% margin on every side:
/// 0.0 = left/top edge, 1.0 = right/bottom edge (values outside are clamped).
/// `width` is a fraction of output width; height follows the webcam's aspect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PipLayout {
    pub visible: bool,
    pub x: f32,
    pub y: f32,
    pub width: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl PipLayout {
    pub fn preset(corner: Corner, width: f32) -> PipLayout {
        let (x, y) = match corner {
            Corner::TopLeft => (0.0, 0.0),
            Corner::TopRight => (1.0, 0.0),
            Corner::BottomLeft => (0.0, 1.0),
            Corner::BottomRight => (1.0, 1.0),
        };
        PipLayout {
            visible: true,
            x,
            y,
            width,
        }
    }
}

impl Default for PipLayout {
    fn default() -> Self {
        PipLayout::preset(Corner::BottomRight, 0.25)
    }
}

/// Pixel rect `(x, y, w, h)` of the PIP, always fully inside the output.
/// `None` when hidden or degenerate.
pub fn pip_rect(
    layout: &PipLayout,
    cam_w: u32,
    cam_h: u32,
    out_w: u32,
    out_h: u32,
) -> Option<(u32, u32, u32, u32)> {
    if !layout.visible || cam_w == 0 || cam_h == 0 || out_w == 0 || out_h == 0 {
        return None;
    }
    let mx = (out_w as f32 * MARGIN).round() as u32;
    let my = (out_h as f32 * MARGIN).round() as u32;
    let (avail_w, avail_h) = (out_w.saturating_sub(2 * mx), out_h.saturating_sub(2 * my));
    let aspect = cam_h as f32 / cam_w as f32;
    let mut w = (layout.width.max(0.0) * out_w as f32).round();
    let mut h = (w * aspect).round();
    // Too big for the frame: shrink, keeping aspect.
    if w > avail_w as f32 || h > avail_h as f32 {
        let s = (avail_w as f32 / w).min(avail_h as f32 / h);
        w = (w * s).floor();
        h = (h * s).floor().min(avail_h as f32);
    }
    let (w, h) = (w as u32, h as u32);
    if w == 0 || h == 0 {
        return None;
    }
    let x = mx + ((layout.x.clamp(0.0, 1.0) * (avail_w - w) as f32).round() as u32);
    let y = my + ((layout.y.clamp(0.0, 1.0) * (avail_h - h) as f32).round() as u32);
    Some((x, y, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_bottom_right() {
        let r = pip_rect(
            &PipLayout::preset(Corner::BottomRight, 0.25),
            1280,
            720,
            1920,
            1080,
        )
        .unwrap();
        assert_eq!(r, (1402, 788, 480, 270));
    }

    #[test]
    fn preset_top_left_uses_margin() {
        let r = pip_rect(
            &PipLayout::preset(Corner::TopLeft, 0.25),
            1280,
            720,
            1920,
            1080,
        )
        .unwrap();
        assert_eq!(r, (38, 22, 480, 270));
    }

    #[test]
    fn pip_clamped_inside_frame() {
        for (x, y) in [(0.95, 0.95), (1.5, 2.0), (-1.0, -0.5)] {
            let l = PipLayout {
                visible: true,
                x,
                y,
                width: 0.25,
            };
            let (px, py, w, h) = pip_rect(&l, 1280, 720, 1920, 1080).unwrap();
            assert!(
                px + w <= 1920 && py + h <= 1080,
                "{x},{y} -> {px},{py} {w}x{h}"
            );
        }
        // Wider than the frame: shrink to fit.
        let l = PipLayout {
            visible: true,
            x: 0.0,
            y: 0.0,
            width: 3.0,
        };
        let (px, py, w, h) = pip_rect(&l, 1280, 720, 1920, 1080).unwrap();
        assert!(px + w <= 1920 && py + h <= 1080);
    }

    #[test]
    fn hidden_pip_has_no_rect() {
        let l = PipLayout {
            visible: false,
            ..PipLayout::default()
        };
        assert!(pip_rect(&l, 1280, 720, 1920, 1080).is_none());
        assert!(pip_rect(&PipLayout::default(), 0, 0, 1920, 1080).is_none());
    }
}
