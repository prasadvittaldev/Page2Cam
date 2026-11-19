//! Picture-in-picture placement and backdrops.
use std::sync::Arc;

use p2c_core::Frame;

/// Fraction of the output width/height kept clear around the PIP.
const MARGIN: f32 = 0.02;
/// How far a full-size cut-out may slide past each edge, as a fraction of its width.
const FULL_OVERHANG: f32 = 0.25;

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
    pub style: OverlayStyle,
}

/// How the webcam is drawn over the page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverlayStyle {
    /// Rectangle, room and all (alpha ignored).
    #[default]
    Box,
    /// Person only (alpha = cut-out mask), standing on the bottom edge.
    CutoutSmall,
    /// Person only, full output height, like a call-app background.
    CutoutFull,
}

impl OverlayStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            OverlayStyle::Box => "box",
            OverlayStyle::CutoutSmall => "cutout-small",
            OverlayStyle::CutoutFull => "cutout-full",
        }
    }

    pub fn parse(s: &str) -> Option<OverlayStyle> {
        match s {
            "box" => Some(OverlayStyle::Box),
            "cutout-small" => Some(OverlayStyle::CutoutSmall),
            "cutout-full" => Some(OverlayStyle::CutoutFull),
            _ => None,
        }
    }

    /// True for the styles that use the cut-out mask.
    pub fn is_cutout(self) -> bool {
        self != OverlayStyle::Box
    }
}

/// What fills the whole output behind the webcam overlay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FullBackdrop {
    /// The web page (as before backdrops existed).
    #[default]
    Page,
    /// The webcam picture, cover-scaled to the output and blurred, instead of
    /// the page. Without a fresh mask the whole frame is blurred (person too).
    BlurredRoom,
}

/// What is drawn behind the person inside the Box style's rectangle. Cut-out
/// styles have no box and ignore it.
#[derive(Clone, Debug, Default)]
pub enum BoxBackdrop {
    /// The webcam's own room (as before backdrops existed).
    #[default]
    Room,
    /// The room blurred. Without a fresh mask the whole box is blurred.
    Blur,
    /// A picture, cover-scaled to the box. Without a fresh mask: the raw room.
    Image(Arc<Frame>),
    /// An opaque colour, `[r, g, b]`. Without a fresh mask: the raw room.
    Colour([u8; 3]),
}

/// Images compare by identity (a new upload is a new `Arc`), not by content.
impl PartialEq for BoxBackdrop {
    fn eq(&self, o: &BoxBackdrop) -> bool {
        match (self, o) {
            (BoxBackdrop::Room, BoxBackdrop::Room) | (BoxBackdrop::Blur, BoxBackdrop::Blur) => true,
            (BoxBackdrop::Image(a), BoxBackdrop::Image(b)) => Arc::ptr_eq(a, b),
            (BoxBackdrop::Colour(a), BoxBackdrop::Colour(b)) => a == b,
            _ => false,
        }
    }
}

/// Backgrounds and edge quality, shared like [`PipLayout`] and read each tick.
/// The default draws exactly what the compositor drew before backdrops.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Backdrop {
    pub full: FullBackdrop,
    pub in_box: BoxBackdrop,
    /// Matte from guided upsampling of the mask (webcam as guide) instead of
    /// bilinear upscaling plus the mask's `EdgeRange` LUT.
    pub refined_edges: bool,
}

impl Backdrop {
    /// Whether drawing `layout` with this backdrop uses the person mask (so
    /// the segmenter should run).
    pub fn needs_mask(&self, layout: &PipLayout) -> bool {
        layout.style.is_cutout()
            || self.full != FullBackdrop::Page
            || self.in_box != BoxBackdrop::Room
    }
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
            style: OverlayStyle::Box,
        }
    }
}

impl Default for PipLayout {
    fn default() -> Self {
        PipLayout::preset(Corner::BottomRight, 0.25)
    }
}

/// Pixel rect `(x, y, w, h)` of the PIP. Inside the output, except that a
/// full-size cut-out may overhang the left/right edge (x < 0 or x + w > out_w).
/// Box: 2% margins, `x`/`y` place it. Cut-out small: side margins, bottom
/// edge on the frame's bottom, `y` ignored. Cut-out full: full height
/// (shrunk to fit the width), bottom-anchored, `x` slides it, `width` ignored.
/// `None` when hidden or degenerate.
pub fn pip_rect(
    layout: &PipLayout,
    cam_w: u32,
    cam_h: u32,
    out_w: u32,
    out_h: u32,
) -> Option<(i32, u32, u32, u32)> {
    if !layout.visible || cam_w == 0 || cam_h == 0 || out_w == 0 || out_h == 0 {
        return None;
    }
    let aspect = cam_h as f32 / cam_w as f32;
    let (mx, my) = match layout.style {
        OverlayStyle::Box => (
            (out_w as f32 * MARGIN).round() as u32,
            (out_h as f32 * MARGIN).round() as u32,
        ),
        OverlayStyle::CutoutSmall => ((out_w as f32 * MARGIN).round() as u32, 0),
        OverlayStyle::CutoutFull => (0, 0),
    };
    let (avail_w, avail_h) = (out_w.saturating_sub(2 * mx), out_h.saturating_sub(2 * my));
    let mut w = match layout.style {
        OverlayStyle::CutoutFull => (out_h as f32 / aspect).round(),
        _ => (layout.width.max(0.0) * out_w as f32).round(),
    };
    let mut h = (w * aspect).round();
    if layout.style == OverlayStyle::CutoutFull {
        h = out_h as f32;
    }
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
    let x = match layout.style {
        // May overhang each side by a quarter of its width, so it can be moved
        // even when it fills the width (a 16:9 camera on a 16:9 output).
        OverlayStyle::CutoutFull => {
            let over = w as f32 * FULL_OVERHANG;
            (-over + layout.x.clamp(0.0, 1.0) * ((avail_w - w) as f32 + 2.0 * over)).round() as i32
        }
        _ => (mx + ((layout.x.clamp(0.0, 1.0) * (avail_w - w) as f32).round() as u32)) as i32,
    };
    let y = match layout.style {
        OverlayStyle::Box => {
            my + ((layout.y.clamp(0.0, 1.0) * (avail_h - h) as f32).round() as u32)
        }
        // Cut-outs stand on the bottom edge.
        _ => out_h - h,
    };
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
                style: OverlayStyle::Box,
            };
            let (px, py, w, h) = pip_rect(&l, 1280, 720, 1920, 1080).unwrap();
            assert!(
                px >= 0 && px as u32 + w <= 1920 && py + h <= 1080,
                "{x},{y} -> {px},{py} {w}x{h}"
            );
        }
        // Wider than the frame: shrink to fit.
        let l = PipLayout {
            visible: true,
            x: 0.0,
            y: 0.0,
            width: 3.0,
            style: OverlayStyle::Box,
        };
        let (px, py, w, h) = pip_rect(&l, 1280, 720, 1920, 1080).unwrap();
        assert!(px >= 0 && px as u32 + w <= 1920 && py + h <= 1080);
    }

    fn cutout(style: OverlayStyle, x: f32, width: f32) -> PipLayout {
        PipLayout {
            visible: true,
            x,
            y: 0.0,
            width,
            style,
        }
    }

    #[test]
    fn style_names_round_trip() {
        for s in [
            OverlayStyle::Box,
            OverlayStyle::CutoutSmall,
            OverlayStyle::CutoutFull,
        ] {
            assert_eq!(OverlayStyle::parse(s.as_str()), Some(s));
        }
        assert_eq!(OverlayStyle::parse("nope"), None);
    }

    #[test]
    fn cutout_small_sits_on_bottom_edge() {
        let r = pip_rect(
            &cutout(OverlayStyle::CutoutSmall, 1.0, 0.25),
            1280,
            720,
            1920,
            1080,
        );
        assert_eq!(r, Some((1402, 810, 480, 270)));
    }

    #[test]
    fn cutout_full_covers_height_centered() {
        let r = pip_rect(
            &cutout(OverlayStyle::CutoutFull, 0.5, 0.1),
            640,
            480,
            1920,
            1080,
        );
        assert_eq!(r, Some((240, 0, 1440, 1080)));
    }

    #[test]
    fn cutout_full_16x9_slides_past_edges() {
        // A widescreen camera fills the width; it may slide a quarter of its
        // width off either side so the operator can still move themselves.
        let at = |x| {
            pip_rect(
                &cutout(OverlayStyle::CutoutFull, x, 0.1),
                1280,
                720,
                1920,
                1080,
            )
        };
        assert_eq!(at(0.0), Some((-480, 0, 1920, 1080)));
        assert_eq!(at(0.5), Some((0, 0, 1920, 1080)));
        assert_eq!(at(1.0), Some((480, 0, 1920, 1080)));
    }

    #[test]
    fn cutout_full_wide_cam_shrinks() {
        let (x, y, w, h) = pip_rect(
            &cutout(OverlayStyle::CutoutFull, 0.5, 0.1),
            2560,
            720,
            1920,
            1080,
        )
        .unwrap();
        assert_eq!((x, w), (0, 1920));
        assert_eq!(y + h, 1080);
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
