use std::sync::Arc;

use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use p2c_core::{Frame, Mask};

use crate::{pip_rect, OverlayStyle, PipLayout};

/// The webcam overlay for one compose: the latest webcam frame, the latest
/// cut-out mask (if any) and where to draw it.
#[derive(Clone, Copy)]
pub struct Pip<'a> {
    pub cam: &'a Arc<Frame>,
    /// Person mask for cut-out styles. A cut-out style without a mask is drawn
    /// as a Box (never a frozen or missing cut-out). Ignored by Box.
    pub mask: Option<&'a Arc<Mask>>,
    pub layout: &'a PipLayout,
}

/// Draws the page and the webcam overlay into the output frame.
///
/// The webcam and the mask are combined here, at draw time, so the person
/// moves at the camera's frame rate even when masks arrive more slowly.
/// Scaled page/webcam/mask layers are cached and only rebuilt when their
/// input changes; when nothing changed at all, `compose` does no work.
pub struct Compositor {
    resizer: Resizer,
    page: Option<ScaledPage>,
    layer: Option<Layer>,
    last: Option<Inputs>,
}

/// The page scaled to fit the output.
struct ScaledPage {
    src: Arc<Frame>,
    w: u32,
    h: u32,
    data: Vec<u8>,
}

/// The webcam scaled to the PIP rectangle, premultiplied by the scaled mask
/// (A = coverage). `opaque` when there is no mask (every A = 255).
struct Layer {
    cam: Arc<Frame>,
    mask: Option<Arc<Mask>>,
    w: u32,
    h: u32,
    opaque: bool,
    data: Vec<u8>,
    mask_buf: Vec<u8>,
}

/// What the last compose was made from (identity of the shared inputs).
struct Inputs {
    page: Option<Arc<Frame>>,
    cam: Option<Arc<Frame>>,
    mask: Option<Arc<Mask>>,
    layout: Option<PipLayout>,
    out: (u32, u32),
}

fn same<T>(a: &Option<Arc<T>>, b: Option<&Arc<T>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

/// Webcam, mask actually used, and layout actually drawn.
type EffectivePip<'a> = (&'a Arc<Frame>, Option<&'a Arc<Mask>>, PipLayout);

/// Cut-out styles need a mask; without one the webcam is drawn as a Box.
fn effective(p: Pip) -> EffectivePip {
    let mut layout = *p.layout;
    let mask = if layout.style.is_cutout() {
        p.mask
    } else {
        None
    };
    if layout.style.is_cutout() && mask.is_none() {
        layout.style = OverlayStyle::Box;
    }
    (p.cam, mask, layout)
}

impl Default for Compositor {
    fn default() -> Self {
        Self::new()
    }
}

impl Compositor {
    pub fn new() -> Compositor {
        Compositor {
            resizer: Resizer::new(),
            page: None,
            layer: None,
            last: None,
        }
    }

    /// Draw `page` into `out`, then the webcam `pip` on top if given and
    /// visible. With no page, `out` becomes black. When the sizes match, the
    /// page is copied; otherwise it is scaled to fit, keeping its aspect
    /// ratio, centered with black bars. Returns false (and leaves `out` as it
    /// is) when the inputs are the same as last time.
    pub fn compose(
        &mut self,
        page: Option<&Arc<Frame>>,
        pip: Option<Pip>,
        out: &mut Frame,
    ) -> bool {
        let pip = pip.map(effective);
        if self.same_inputs(page, pip, (out.width, out.height)) {
            return false;
        }
        self.draw_page(page, out);
        if let Some((cam, mask, layout)) = pip {
            if let Some((x, y, w, h)) =
                pip_rect(&layout, cam.width, cam.height, out.width, out.height)
            {
                self.update_layer(cam, mask, w, h);
                let layer = self.layer.as_ref().expect("layer just built");
                if layer.opaque {
                    blit_clipped(&layer.data, w, h, out, x, y);
                } else {
                    blend_premultiplied(&layer.data, w, h, out, x, y);
                }
            }
        }
        self.last = Some(Inputs {
            page: page.cloned(),
            cam: pip.map(|p| p.0.clone()),
            mask: pip.and_then(|p| p.1.cloned()),
            layout: pip.map(|p| p.2),
            out: (out.width, out.height),
        });
        true
    }

    /// Whether [`Compositor::compose`] would return false for these inputs
    /// and a `width` x `height` output (lets callers skip getting a target).
    pub fn is_unchanged(
        &self,
        page: Option<&Arc<Frame>>,
        pip: Option<Pip>,
        width: u32,
        height: u32,
    ) -> bool {
        self.same_inputs(page, pip.map(effective), (width, height))
    }

    fn same_inputs(
        &self,
        page: Option<&Arc<Frame>>,
        pip: Option<EffectivePip>,
        out: (u32, u32),
    ) -> bool {
        self.last.as_ref().is_some_and(|l| {
            same(&l.page, page)
                && same(&l.cam, pip.map(|p| p.0))
                && same(&l.mask, pip.and_then(|p| p.1))
                && l.layout == pip.map(|p| p.2)
                && l.out == out
        })
    }

    fn draw_page(&mut self, page: Option<&Arc<Frame>>, out: &mut Frame) {
        let Some(page) = page.filter(|p| p.width > 0 && p.height > 0) else {
            fill_black(out);
            return;
        };
        if page.width == out.width && page.height == out.height {
            out.data.copy_from_slice(&page.data);
            return;
        }
        let (dx, dy, dw, dh) = fit_rect(page.width, page.height, out.width, out.height);
        fill_black(out);
        if dw == 0 || dh == 0 {
            return;
        }
        let fresh = self
            .page
            .as_ref()
            .is_some_and(|s| Arc::ptr_eq(&s.src, page) && (s.w, s.h) == (dw, dh));
        if !fresh {
            let mut data = self.page.take().map(|s| s.data).unwrap_or_default();
            data.resize(dw as usize * dh as usize * 4, 0);
            resize_into(&mut self.resizer, page, &mut data, dw, dh, true);
            self.page = Some(ScaledPage {
                src: page.clone(),
                w: dw,
                h: dh,
                data,
            });
        }
        let s = self.page.as_ref().expect("page just scaled");
        blit(&s.data, dw, dh, out, dx, dy);
    }

    /// Rebuild the webcam layer if the webcam frame, the mask or the size changed.
    fn update_layer(&mut self, cam: &Arc<Frame>, mask: Option<&Arc<Mask>>, w: u32, h: u32) {
        let fresh = self.layer.as_ref().is_some_and(|l| {
            Arc::ptr_eq(&l.cam, cam) && same(&l.mask, mask) && (l.w, l.h) == (w, h)
        });
        if fresh {
            return;
        }
        let (mut data, mut mask_buf) = self
            .layer
            .take()
            .map(|l| (l.data, l.mask_buf))
            .unwrap_or_default();
        let px = w as usize * h as usize;
        data.resize(px * 4, 0);
        // Straight scaling: the webcam's alpha byte is not transparency.
        resize_into(&mut self.resizer, cam, &mut data, w, h, false);
        let opaque = match mask {
            Some(m) => {
                mask_buf.resize(px, 0);
                scale_mask_into(&mut self.resizer, m, &mut mask_buf, w, h);
                let lut = m.edge.lut();
                for (p, a) in data.as_chunks_mut::<4>().0.iter_mut().zip(&mask_buf) {
                    let a = lut[*a as usize] as u32;
                    for c in &mut p[..3] {
                        *c = ((*c as u32 * a + 127) / 255) as u8;
                    }
                    p[3] = a as u8;
                }
                false
            }
            None => {
                for p in data.as_chunks_mut::<4>().0 {
                    p[3] = 255;
                }
                true
            }
        };
        self.layer = Some(Layer {
            cam: cam.clone(),
            mask: mask.cloned(),
            w,
            h,
            opaque,
            data,
            mask_buf,
        });
    }
}

/// Largest rect `(x, y, w, h)` with the source aspect ratio that fits in the
/// output, centered (the page letterbox; the GPU compositor uses it too).
pub fn fit_rect(sw: u32, sh: u32, ow: u32, oh: u32) -> (u32, u32, u32, u32) {
    let scale = (ow as f64 / sw as f64).min(oh as f64 / sh as f64);
    let dw = ((sw as f64 * scale).round() as u32).min(ow);
    let dh = ((sh as f64 * scale).round() as u32).min(oh);
    ((ow - dw) / 2, (oh - dh) / 2, dw, dh)
}

fn bilinear() -> ResizeOptions {
    ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
}

/// Scale a BGRA frame into `dst` (`dw * dh * 4` bytes). With `use_alpha`
/// false the alpha byte is scaled as plain data.
fn resize_into(r: &mut Resizer, src: &Frame, dst: &mut [u8], dw: u32, dh: u32, use_alpha: bool) {
    let src_img = ImageRef::new(src.width, src.height, &src.data, PixelType::U8x4)
        .expect("Frame invariant: len == w*h*4");
    let mut dst_img =
        Image::from_slice_u8(dw, dh, dst, PixelType::U8x4).expect("buffer sized by caller");
    r.resize(&src_img, &mut dst_img, &bilinear().use_alpha(use_alpha))
        .expect("same pixel type on both sides");
}

/// Scale a one-byte-per-pixel mask into `dst` (`dw * dh` bytes).
fn scale_mask_into(r: &mut Resizer, m: &Mask, dst: &mut [u8], dw: u32, dh: u32) {
    let src_img = ImageRef::new(m.width, m.height, &m.data, PixelType::U8)
        .expect("Mask invariant: len == w*h");
    let mut dst_img =
        Image::from_slice_u8(dw, dh, dst, PixelType::U8).expect("buffer sized by caller");
    r.resize(&src_img, &mut dst_img, &bilinear())
        .expect("same pixel type on both sides");
}

/// Scale a frame to exactly `dw` x `dh` (bilinear).
pub fn scale_frame(src: &Frame, dw: u32, dh: u32) -> Frame {
    let mut data = vec![0u8; dw as usize * dh as usize * 4];
    resize_into(&mut Resizer::new(), src, &mut data, dw, dh, true);
    Frame {
        width: dw,
        height: dh,
        data,
    }
}

/// Scale a frame to exactly `dw` x `dh` (bilinear), treating the alpha byte
/// as plain data rather than transparency (colours never darken where A < 255).
pub fn scale_frame_straight(src: &Frame, dw: u32, dh: u32) -> Frame {
    let mut data = vec![0u8; dw as usize * dh as usize * 4];
    resize_into(&mut Resizer::new(), src, &mut data, dw, dh, false);
    Frame {
        width: dw,
        height: dh,
        data,
    }
}

/// Visible source columns `first..last` of a `w`-wide layer drawn at `x`.
fn visible_cols(x: i32, w: u32, out_w: u32) -> Option<(usize, usize, usize)> {
    let first = (-x).max(0) as usize;
    let last = (out_w as i64 - x as i64).clamp(0, w as i64) as usize;
    (first < last).then(|| (first, last, x.max(0) as usize))
}

/// Draw a premultiplied BGRA layer (A = coverage) over `out` at (x, y),
/// clipping columns that fall outside the output.
fn blend_premultiplied(src: &[u8], w: u32, h: u32, out: &mut Frame, x: i32, y: u32) {
    let Some((first, last, dx)) = visible_cols(x, w, out.width) else {
        return;
    };
    let row = w as usize * 4;
    let ostride = out.width as usize * 4;
    for r in 0..h as usize {
        let o = (y as usize + r) * ostride + dx * 4;
        let dst = out.data[o..o + (last - first) * 4].as_chunks_mut::<4>().0;
        let s = src[r * row + first * 4..r * row + last * 4]
            .as_chunks::<4>()
            .0;
        for (d, s) in dst.iter_mut().zip(s) {
            match s[3] {
                0 => {}
                255 => d[..3].copy_from_slice(&s[..3]),
                a => {
                    let inv = 255 - a as u32;
                    for c in 0..3 {
                        d[c] = (s[c] as u32 + (d[c] as u32 * inv + 127) / 255).min(255) as u8;
                    }
                }
            }
        }
    }
}

/// Copy an opaque BGRA layer into `out` at (x, y), clipping columns outside.
fn blit_clipped(src: &[u8], w: u32, h: u32, out: &mut Frame, x: i32, y: u32) {
    let Some((first, last, dx)) = visible_cols(x, w, out.width) else {
        return;
    };
    let row = w as usize * 4;
    let ostride = out.width as usize * 4;
    for r in 0..h as usize {
        let o = (y as usize + r) * ostride + dx * 4;
        out.data[o..o + (last - first) * 4]
            .copy_from_slice(&src[r * row + first * 4..r * row + last * 4]);
    }
}

/// Copy a tightly packed BGRA `src` (w x h) into `out` at (x, y), opaque
/// (the output never carries a source's alpha). Caller guarantees it fits.
pub(crate) fn blit(src: &[u8], w: u32, h: u32, out: &mut Frame, x: u32, y: u32) {
    let row = w as usize * 4;
    let ostride = out.width as usize * 4;
    for r in 0..h as usize {
        let o = (y as usize + r) * ostride + x as usize * 4;
        let dst = &mut out.data[o..o + row];
        dst.copy_from_slice(&src[r * row..(r + 1) * row]);
        for px in dst.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
    }
}

fn fill_black(out: &mut Frame) {
    for px in out.data.as_chunks_mut::<4>().0 {
        *px = [0, 0, 0, 255];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2c_core::{EdgeRange, Frame, Mask};
    use std::sync::Arc;

    const RED: [u8; 4] = [0, 0, 255, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];
    const BLUE: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Arc<Frame> {
        Arc::new(Frame::from_bgra(w, h, px.repeat((w * h) as usize)).unwrap())
    }

    fn mask(w: u32, h: u32, data: Vec<u8>) -> Arc<Mask> {
        Arc::new(Mask::new(w, h, data).unwrap())
    }

    fn full_layout(style: crate::OverlayStyle) -> crate::PipLayout {
        crate::PipLayout {
            visible: true,
            x: 0.5,
            y: 0.0,
            width: 1.0,
            style,
        }
    }

    /// One compose with a fresh compositor.
    fn once(page: Option<&Arc<Frame>>, pip: Option<Pip>, out: &mut Frame) {
        Compositor::new().compose(page, pip, out);
    }

    #[test]
    fn compose_draws_pip_over_page() {
        let mut out = Frame::new_black(64, 36);
        let layout = crate::PipLayout {
            visible: true,
            x: 0.0,
            y: 0.0,
            width: 0.25,
            style: crate::OverlayStyle::Box,
        };
        let cam = solid(16, 9, BLUE);
        let pip = Pip {
            cam: &cam,
            mask: None,
            layout: &layout,
        };
        once(Some(&solid(64, 36, RED)), Some(pip), &mut out);
        assert_eq!(out.pixel(2, 2), BLUE);
        assert_eq!(out.pixel(40, 20), RED);
    }

    #[test]
    fn mask_is_the_cutout_alpha() {
        // 3x1 white page, opaque black cam, mask 0 / 128 / 255, full-size cut-out.
        let mut out = Frame::new_black(3, 1);
        let cam = solid(3, 1, BLACK);
        let m = mask(3, 1, vec![0, 128, 255]);
        let l = full_layout(crate::OverlayStyle::CutoutFull);
        let pip = Pip {
            cam: &cam,
            mask: Some(&m),
            layout: &l,
        };
        once(Some(&solid(3, 1, WHITE)), Some(pip), &mut out);
        assert_eq!(out.pixel(0, 0), WHITE);
        assert!(
            (out.pixel(1, 0)[0] as i32 - 127).abs() <= 1,
            "{:?}",
            out.pixel(1, 0)
        );
        assert_eq!(out.pixel(2, 0), BLACK);
    }

    #[test]
    fn low_res_mask_is_scaled_to_the_pip() {
        // A 2x1 mask (left: room, right: person) over a 64x36 full cut-out.
        let mut out = Frame::new_black(64, 36);
        let cam = solid(16, 9, BLACK);
        let m = mask(2, 1, vec![0, 255]);
        let l = full_layout(crate::OverlayStyle::CutoutFull);
        let pip = Pip {
            cam: &cam,
            mask: Some(&m),
            layout: &l,
        };
        once(Some(&solid(64, 36, WHITE)), Some(pip), &mut out);
        assert_eq!(out.pixel(2, 18), WHITE, "left = page");
        assert_eq!(out.pixel(61, 18), BLACK, "right = person");
    }

    #[test]
    fn edge_range_sharpens_the_mask() {
        // Mask value 100 is "mostly room" once the 64..192 edge range is applied.
        let mut out = Frame::new_black(1, 1);
        let cam = solid(1, 1, BLACK);
        let mut m = Mask::new(1, 1, vec![100]).unwrap();
        m.edge = EdgeRange { lo: 64, hi: 192 };
        let m = Arc::new(m);
        let l = full_layout(crate::OverlayStyle::CutoutFull);
        let pip = Pip {
            cam: &cam,
            mask: Some(&m),
            layout: &l,
        };
        once(Some(&solid(1, 1, WHITE)), Some(pip), &mut out);
        let v = out.pixel(0, 0)[0];
        assert!(v > 200, "page should dominate, got {v}");
    }

    #[test]
    fn new_webcam_frame_shows_without_a_new_mask() {
        // The person must move at camera speed, not at mask speed.
        let mut c = Compositor::new();
        let mut out = Frame::new_black(4, 4);
        let page = solid(4, 4, WHITE);
        let m = mask(1, 1, vec![255]);
        let l = full_layout(crate::OverlayStyle::CutoutFull);
        let blue = solid(4, 4, BLUE);
        c.compose(
            Some(&page),
            Some(Pip {
                cam: &blue,
                mask: Some(&m),
                layout: &l,
            }),
            &mut out,
        );
        assert_eq!(out.pixel(2, 2), BLUE);
        let green = solid(4, 4, GREEN);
        c.compose(
            Some(&page),
            Some(Pip {
                cam: &green,
                mask: Some(&m),
                layout: &l,
            }),
            &mut out,
        );
        assert_eq!(out.pixel(2, 2), GREEN);
    }

    #[test]
    fn cutout_without_mask_draws_a_box() {
        // Raw webcam alpha is meaningless (0 here); without a mask it must be opaque.
        let mut out = Frame::new_black(3, 1);
        let cam = Arc::new(Frame::from_bgra(3, 1, [255, 0, 0, 0].repeat(3)).unwrap());
        let l = full_layout(crate::OverlayStyle::CutoutFull);
        let pip = Pip {
            cam: &cam,
            mask: None,
            layout: &l,
        };
        once(Some(&solid(3, 1, WHITE)), Some(pip), &mut out);
        assert_eq!(out.pixel(1, 0), BLUE);
    }

    #[test]
    fn box_ignores_mask() {
        let mut out = Frame::new_black(3, 1);
        let cam = solid(3, 1, BLACK);
        let m = mask(1, 1, vec![0]);
        let mut l = full_layout(crate::OverlayStyle::Box);
        l.x = 0.0;
        // Box keeps a 2% margin; on a 3 px frame that rounds to 0.
        let pip = Pip {
            cam: &cam,
            mask: Some(&m),
            layout: &l,
        };
        once(Some(&solid(3, 1, WHITE)), Some(pip), &mut out);
        assert_eq!(out.pixel(1, 0), BLACK);
    }

    #[test]
    fn unchanged_inputs_skip_work() {
        let mut c = Compositor::new();
        let mut out = Frame::new_black(4, 4);
        let page = solid(4, 4, RED);
        assert!(c.compose(Some(&page), None, &mut out));
        assert!(
            !c.compose(Some(&page), None, &mut out),
            "same inputs redrawn"
        );
        assert_eq!(out.pixel(1, 1), RED);
        let page2 = solid(4, 4, BLUE);
        assert!(c.compose(Some(&page2), None, &mut out));
        assert_eq!(out.pixel(1, 1), BLUE);
        // A layout change alone must redraw too.
        let cam = solid(2, 2, GREEN);
        let l = full_layout(crate::OverlayStyle::Box);
        let pip = || {
            Some(Pip {
                cam: &cam,
                mask: None,
                layout: &l,
            })
        };
        assert!(c.compose(Some(&page2), pip(), &mut out));
        assert!(!c.compose(Some(&page2), pip(), &mut out));
    }

    #[test]
    fn cutout_full_clips_at_frame_edges() {
        // 16:9 cam slid fully left: the left quarter of the cam is off-frame.
        let mut out = Frame::new_black(64, 36);
        let mut cam = Frame::from_bgra(16, 9, BLACK.repeat(16 * 9)).unwrap();
        for y in 0..9 {
            for x in 0..4 {
                let i = (y * 16 + x) * 4;
                cam.data[i..i + 4].copy_from_slice(&RED); // red left quarter
            }
        }
        let cam = Arc::new(cam);
        let m = mask(1, 1, vec![255]);
        let mut l = full_layout(crate::OverlayStyle::CutoutFull);
        l.x = 0.0;
        let pip = Pip {
            cam: &cam,
            mask: Some(&m),
            layout: &l,
        };
        once(Some(&solid(64, 36, WHITE)), Some(pip), &mut out);
        assert_eq!(
            out.pixel(2, 18)[..3],
            [0, 0, 0],
            "left edge should show the cam's 2nd quarter"
        );
        assert_eq!(out.pixel(63, 18), WHITE, "right quarter stays page");
    }

    #[test]
    fn scale_frame_resizes() {
        let f = scale_frame(&solid(8, 8, RED), 4, 2);
        assert_eq!((f.width, f.height, f.data.len()), (4, 2, 32));
        assert_eq!(f.pixel(1, 1), RED);
    }

    #[test]
    fn tick_without_page_emits_black() {
        let mut out = Frame::from_bgra(8, 4, RED.repeat(32)).unwrap();
        once(None, None, &mut out);
        for (x, y) in [(0, 0), (4, 2), (7, 3)] {
            assert_eq!(out.pixel(x, y), BLACK);
        }
    }

    #[test]
    fn compose_same_size_copies() {
        let mut out = Frame::new_black(4, 4);
        once(Some(&solid(4, 4, RED)), None, &mut out);
        assert_eq!(out.pixel(2, 2), RED);
    }

    #[test]
    fn tick_scales_mismatched_page() {
        let mut out = Frame::new_black(8, 4);
        once(Some(&solid(2, 2, RED)), None, &mut out);
        assert_eq!((out.width, out.height, out.data.len()), (8, 4, 128));
        assert_eq!(out.pixel(4, 2), RED);
        assert_eq!(out.pixel(0, 2), BLACK);
    }
}
