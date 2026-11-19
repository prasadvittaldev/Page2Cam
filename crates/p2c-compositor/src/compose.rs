use std::sync::Arc;

use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use p2c_core::{DurationStats, Frame, Mask, Summary};

use crate::{
    pip_rect, Backdrop, BlurCache, BoxBackdrop, FullBackdrop, GuidedScratch, OverlayStyle,
    PipLayout,
};

/// Guided-filter window radius, in pixels of the refined matte as computed:
/// for a PIP wider than [`REFINED_MAX_W`] that is the capped size, so the
/// radius covers proportionally more of the drawn PIP.
pub const GUIDED_RADIUS: u32 = 40;
/// Guided-filter regularisation (guide and mask in 0..1).
pub const GUIDED_EPS: f32 = 1e-4;
/// The refined matte is computed at most this wide, then scaled up bilinearly.
pub const REFINED_MAX_W: u32 = 960;

/// The size the refined matte of a `w` x `h` PIP is computed at (capped to
/// [`REFINED_MAX_W`] wide, aspect kept; then scaled up to `w` x `h`).
pub fn refined_size(w: u32, h: u32) -> (u32, u32) {
    if w > REFINED_MAX_W {
        let gh = (h as u64 * REFINED_MAX_W as u64 / w as u64).max(1) as u32;
        (REFINED_MAX_W, gh)
    } else {
        (w, h)
    }
}

/// The webcam overlay for one compose: the latest webcam frame, the latest
/// cut-out mask (if any) and where to draw it.
#[derive(Clone, Copy)]
pub struct Pip<'a> {
    pub cam: &'a Arc<Frame>,
    /// Person mask for cut-out styles and Box backdrops. Without one, a
    /// cut-out style is drawn as a Box (never a frozen or missing cut-out).
    pub mask: Option<&'a Arc<Mask>>,
    pub layout: &'a PipLayout,
}

/// Why the output doesn't show the chosen backdrop (for the status line).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComposeNote {
    #[default]
    None,
    /// A backdrop needs the person mask and there is no fresh one: the
    /// fallback (raw room, or everything blurred) is showing.
    CutoutNotReady,
    /// The blurred room needs the webcam and there is no fresh frame: the
    /// page is showing instead.
    WebcamOff,
    /// The producer can't draw backdrops or refined edges (the GPU's
    /// backdrop shaders failed to compile): Page/Room with plain edges shows.
    BackdropUnavailable,
}

impl ComposeNote {
    /// The note for these inputs (the same on every producer).
    /// `cam`/`mask`: a fresh webcam frame / mask is available.
    pub fn of(backdrop: &Backdrop, layout: &PipLayout, cam: bool, mask: bool) -> ComposeNote {
        let blurred_room = backdrop.full == FullBackdrop::BlurredRoom;
        if blurred_room && !cam {
            return ComposeNote::WebcamOff;
        }
        if !cam || !layout.visible || mask {
            return ComposeNote::None;
        }
        let box_needs_mask =
            layout.style == OverlayStyle::Box && backdrop.in_box != BoxBackdrop::Room;
        if blurred_room || box_needs_mask {
            ComposeNote::CutoutNotReady
        } else {
            ComposeNote::None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ComposeNote::None => "",
            ComposeNote::CutoutNotReady => "cut-out not ready",
            ComposeNote::WebcamOff => "webcam off: showing the page",
            ComposeNote::BackdropUnavailable => {
                "backgrounds unavailable on this GPU: showing the page and room"
            }
        }
    }
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
    note: ComposeNote,
    /// Blurred webcam for the box (Blur backdrop).
    box_blur: BlurCache,
    /// Blurred webcam for the whole frame (BlurredRoom).
    room_blur: BlurCache,
    image: Option<ScaledImage>,
    guided: GuidedScratch,
    guided_out: Vec<u8>,
    /// Time per refined matte (guided filter + upscale), for the stats line.
    refined_t: DurationStats,
}

/// The page scaled to fit the output.
struct ScaledPage {
    src: Arc<Frame>,
    w: u32,
    h: u32,
    data: Vec<u8>,
}

/// A box backdrop image cover-scaled to the box (opaque).
struct ScaledImage {
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
    refined: bool,
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
    backdrop: Backdrop,
    out: (u32, u32),
}

fn same<T>(a: &Option<Arc<T>>, b: Option<&Arc<T>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

/// What fills the frame behind the PIP.
#[derive(Clone, Copy)]
pub enum PlanBase<'a> {
    Page(Option<&'a Arc<Frame>>),
    /// The webcam, cover-scaled and blurred.
    BlurredRoom(&'a Arc<Frame>),
}

/// How the PIP rectangle is filled.
#[derive(Clone, Copy)]
pub enum PipDraw<'a> {
    /// The webcam as an opaque rectangle (Box, or a cut-out without a mask).
    Opaque,
    /// The person (premultiplied by the matte) over what is beneath.
    Cutout,
    /// The whole box blurred (Blur backdrop without a mask).
    BlurAll,
    /// Box backdrop first, then the person over it.
    Over(&'a BoxBackdrop),
}

/// The PIP actually drawn: webcam, mask used, layout, and how.
#[derive(Clone, Copy)]
pub struct PlannedPip<'a> {
    pub cam: &'a Arc<Frame>,
    /// The mask the matte comes from (`None` for Opaque and BlurAll).
    pub mask: Option<&'a Arc<Mask>>,
    /// The layout drawn (a cut-out without a mask becomes a Box).
    pub layout: PipLayout,
    pub draw: PipDraw<'a>,
}

/// One compose, decided: what goes beneath and what is drawn on top. Both
/// producers (CPU [`Compositor`] and the D3D11 one) draw from this, so every
/// fallback rule lives in [`plan_compose`] only.
#[derive(Clone, Copy)]
pub struct ComposePlan<'a> {
    pub base: PlanBase<'a>,
    pub pip: Option<PlannedPip<'a>>,
}

/// In-box Blur, for a Box under a blurred room.
static BLUR: BoxBackdrop = BoxBackdrop::Blur;

/// Works out what to draw, including every fallback for a missing mask.
/// `page` only travels into [`PlanBase::Page`] (a producer with its own page
/// source passes `None`).
pub fn plan_compose<'a>(
    page: Option<&'a Arc<Frame>>,
    pip: Option<Pip<'a>>,
    b: &'a Backdrop,
) -> ComposePlan<'a> {
    let base = match (b.full, pip) {
        (FullBackdrop::Page, _) => PlanBase::Page(page),
        (FullBackdrop::BlurredRoom, Some(p)) => PlanBase::BlurredRoom(p.cam),
        // Nothing to blur: the page, with a note (`ComposeNote::WebcamOff`).
        (FullBackdrop::BlurredRoom, None) => PlanBase::Page(page),
    };
    let pip = pip.and_then(|p| {
        // Blurred room without a mask: the whole frame stays blurred.
        if b.full == FullBackdrop::BlurredRoom && p.mask.is_none() {
            return None;
        }
        let mut layout = *p.layout;
        let draw = if layout.style.is_cutout() {
            if p.mask.is_some() {
                PipDraw::Cutout
            } else {
                layout.style = OverlayStyle::Box;
                PipDraw::Opaque
            }
        } else {
            // Under a blurred room the box's room is blurred too, even if the
            // backdrop says Room (layout and backdrop are read separately, so
            // a Box can meet a blurred room for a tick): never the sharp room.
            let in_box = match (&b.in_box, b.full) {
                (BoxBackdrop::Room, FullBackdrop::BlurredRoom) => &BLUR,
                (bg, _) => bg,
            };
            match (in_box, p.mask) {
                (BoxBackdrop::Room, _) => PipDraw::Opaque,
                (BoxBackdrop::Blur, None) => PipDraw::BlurAll,
                // Image/Colour without a mask: the raw room (and a note).
                (_, None) => PipDraw::Opaque,
                (bg, Some(_)) => PipDraw::Over(bg),
            }
        };
        let mask = match draw {
            PipDraw::Cutout | PipDraw::Over(_) => p.mask,
            PipDraw::Opaque | PipDraw::BlurAll => None,
        };
        Some(PlannedPip {
            cam: p.cam,
            mask,
            layout,
            draw,
        })
    });
    ComposePlan { base, pip }
}

/// The page drawn (none under a blurred room: its repaints don't matter).
fn base_page<'a>(plan: &ComposePlan<'a>) -> Option<&'a Arc<Frame>> {
    match plan.base {
        PlanBase::Page(p) => p,
        PlanBase::BlurredRoom(_) => None,
    }
}

/// The webcam frame the output depends on (PIP or blurred room).
fn base_cam<'a>(plan: &ComposePlan<'a>) -> Option<&'a Arc<Frame>> {
    match plan.base {
        PlanBase::BlurredRoom(cam) => Some(cam),
        PlanBase::Page(_) => plan.pip.map(|p| p.cam),
    }
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
            note: ComposeNote::None,
            box_blur: BlurCache::new(),
            room_blur: BlurCache::new(),
            image: None,
            guided: GuidedScratch::new(),
            guided_out: Vec::new(),
            refined_t: DurationStats::default(),
        }
    }

    /// Refined-matte times since the last call (`None` if none was made).
    pub fn take_refined_summary(&mut self) -> Option<Summary> {
        self.refined_t.take_summary()
    }

    /// The note for the last compose that redrew (it is not updated when
    /// `compose` returns false). For the live note use the ticker's
    /// `TickerStats::compose_note`, which is computed on every tick.
    pub fn note(&self) -> ComposeNote {
        self.note
    }

    /// Draw `page` (or the backdrop that replaces it) into `out`, then the
    /// webcam `pip` on top if given and visible. With no page, `out` becomes
    /// black. When the sizes match, the page is copied; otherwise it is scaled
    /// to fit, keeping its aspect ratio, centered with black bars. Returns
    /// false (and leaves `out` as it is) when the inputs are the same as last
    /// time. `Backdrop::default()` draws exactly what it did before backdrops.
    pub fn compose(
        &mut self,
        page: Option<&Arc<Frame>>,
        pip: Option<Pip>,
        backdrop: &Backdrop,
        out: &mut Frame,
    ) -> bool {
        let plan = plan_compose(page, pip, backdrop);
        if self.same_inputs(&plan, backdrop, (out.width, out.height)) {
            return false;
        }
        match plan.base {
            PlanBase::Page(page) => self.draw_page(page, out),
            PlanBase::BlurredRoom(cam) => self.draw_blurred_room(cam, out),
        }
        if let Some(p) = plan.pip {
            let (cam, mask, refined) = (p.cam, p.mask, backdrop.refined_edges);
            if let Some(rect) = pip_rect(&p.layout, cam.width, cam.height, out.width, out.height) {
                let (x, y, w, h) = rect;
                match p.draw {
                    PipDraw::BlurAll => {
                        let blurred = self.box_blur.get(cam, w, h);
                        blit_clipped(blurred, w, h, out, x, y);
                    }
                    PipDraw::Opaque | PipDraw::Cutout => {
                        self.draw_layer(cam, mask, refined, rect, out)
                    }
                    PipDraw::Over(bg) => {
                        self.draw_box_backdrop(bg, cam, rect, out);
                        self.draw_layer(cam, mask, refined, rect, out);
                    }
                }
            }
        }
        self.note = ComposeNote::of(
            backdrop,
            pip.map_or(&PipLayout::default(), |p| p.layout),
            pip.is_some(),
            pip.is_some_and(|p| p.mask.is_some()),
        );
        self.last = Some(Inputs {
            page: base_page(&plan).cloned(),
            cam: base_cam(&plan).cloned(),
            mask: plan.pip.and_then(|p| p.mask.cloned()),
            layout: plan.pip.map(|p| p.layout),
            backdrop: backdrop.clone(),
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
        backdrop: &Backdrop,
        width: u32,
        height: u32,
    ) -> bool {
        self.same_inputs(
            &plan_compose(page, pip, backdrop),
            backdrop,
            (width, height),
        )
    }

    /// Every cached layer (scaled page, webcam layer, both blurs, scaled
    /// image) is a function of these, so equal inputs mean an equal output:
    /// a blur is only rebuilt for a new webcam frame or a new size, and both
    /// are compared here.
    fn same_inputs(&self, plan: &ComposePlan, backdrop: &Backdrop, out: (u32, u32)) -> bool {
        self.last.as_ref().is_some_and(|l| {
            same(&l.page, base_page(plan))
                && same(&l.cam, base_cam(plan))
                && same(&l.mask, plan.pip.and_then(|p| p.mask))
                && l.layout == plan.pip.map(|p| p.layout)
                && l.backdrop == *backdrop
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

    /// The webcam cover-scaled to the output (centre kept) and blurred.
    fn draw_blurred_room(&mut self, cam: &Arc<Frame>, out: &mut Frame) {
        let (ow, oh) = (out.width, out.height);
        if cam.width == 0 || cam.height == 0 || ow == 0 || oh == 0 {
            fill_black(out);
            return;
        }
        // Cover-cropped while scaling down, blurred small, upscaled to the output.
        let blurred = self.room_blur.get_cover(cam, ow, oh);
        out.data.copy_from_slice(blurred);
    }

    /// Fill the box with its backdrop (opaque), before the person is drawn.
    fn draw_box_backdrop(
        &mut self,
        bg: &BoxBackdrop,
        cam: &Arc<Frame>,
        (x, y, w, h): (i32, u32, u32, u32),
        out: &mut Frame,
    ) {
        match bg {
            // Never planned: the room is the webcam layer itself.
            BoxBackdrop::Room => {}
            BoxBackdrop::Blur => {
                let blurred = self.box_blur.get(cam, w, h);
                blit_clipped(blurred, w, h, out, x, y);
            }
            BoxBackdrop::Image(img) => {
                self.update_image(img, w, h);
                let s = self.image.as_ref().expect("image just scaled");
                blit_clipped(&s.data, w, h, out, x, y);
            }
            BoxBackdrop::Colour([r, g, b]) => {
                let Some((first, last, dx)) = visible_cols(x, w, out.width) else {
                    return;
                };
                let ostride = out.width as usize * 4;
                for row in 0..h as usize {
                    let o = (y as usize + row) * ostride + dx * 4;
                    out.data[o..o + (last - first) * 4]
                        .as_chunks_mut::<4>()
                        .0
                        .fill([*b, *g, *r, 255]);
                }
            }
        }
    }

    /// Rebuild the cover-scaled box image if the image or the size changed.
    fn update_image(&mut self, img: &Arc<Frame>, w: u32, h: u32) {
        if self
            .image
            .as_ref()
            .is_some_and(|s| Arc::ptr_eq(&s.src, img) && (s.w, s.h) == (w, h))
        {
            return;
        }
        let mut data = self.image.take().map(|s| s.data).unwrap_or_default();
        data.clear();
        data.resize(w as usize * h as usize * 4, 0);
        let valid = img.width > 0
            && img.height > 0
            && img.data.len() == img.width as usize * img.height as usize * 4;
        if valid {
            resize_cover_into(&mut self.resizer, img, &mut data, w, h);
        }
        for p in data.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
        self.image = Some(ScaledImage {
            src: img.clone(),
            w,
            h,
            data,
        });
    }

    /// Draw the webcam layer (opaque, or the person by the matte) at the rect.
    fn draw_layer(
        &mut self,
        cam: &Arc<Frame>,
        mask: Option<&Arc<Mask>>,
        refined: bool,
        (x, y, w, h): (i32, u32, u32, u32),
        out: &mut Frame,
    ) {
        self.update_layer(cam, mask, refined, w, h);
        let layer = self.layer.as_ref().expect("layer just built");
        if layer.opaque {
            blit_clipped(&layer.data, w, h, out, x, y);
        } else {
            blend_premultiplied(&layer.data, w, h, out, x, y);
        }
    }

    /// Rebuild the webcam layer if the webcam frame, the mask, the matte mode
    /// or the size changed.
    fn update_layer(
        &mut self,
        cam: &Arc<Frame>,
        mask: Option<&Arc<Mask>>,
        refined: bool,
        w: u32,
        h: u32,
    ) {
        // Without a mask the matte mode doesn't matter.
        let refined = refined && mask.is_some();
        let fresh = self.layer.as_ref().is_some_and(|l| {
            Arc::ptr_eq(&l.cam, cam)
                && same(&l.mask, mask)
                && l.refined == refined
                && (l.w, l.h) == (w, h)
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
                let lut = if refined {
                    self.refined_matte(m, cam, w, h, &mut mask_buf);
                    // The guided filter replaces the edge LUT.
                    IDENTITY_LUT
                } else {
                    mask_buf.resize(px, 0);
                    scale_mask_into(&mut self.resizer, m, &mut mask_buf, w, h);
                    m.edge.lut()
                };
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
            refined,
            w,
            h,
            opaque,
            data,
            mask_buf,
        });
    }

    /// Guided upsampling of `m` to `w` x `h` (webcam as guide) into `dst`.
    /// Wider than [`REFINED_MAX_W`]: computed at that width, then scaled up.
    fn refined_matte(&mut self, m: &Mask, cam: &Frame, w: u32, h: u32, dst: &mut Vec<u8>) {
        let t0 = std::time::Instant::now();
        let (gw, gh) = refined_size(w, h);
        self.guided.run(
            m,
            cam,
            gw,
            gh,
            GUIDED_RADIUS,
            GUIDED_EPS,
            &mut self.guided_out,
        );
        if (gw, gh) == (w, h) {
            std::mem::swap(dst, &mut self.guided_out);
        } else {
            dst.clear();
            dst.resize(w as usize * h as usize, 0);
            scale_u8_into(&mut self.resizer, &self.guided_out, gw, gh, dst, w, h);
        }
        self.refined_t.record(t0.elapsed());
    }
}

/// Mask values used as they are (refined mattes skip the edge LUT).
const IDENTITY_LUT: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = i as u8;
        i += 1;
    }
    t
};

/// The centred crop `(left, top, width, height)` of a `sw` x `sh` source
/// that has the `dw` x `dh` aspect ratio (CSS `object-fit: cover`), in
/// source pixels (fractional).
pub fn cover_crop(sw: u32, sh: u32, dw: u32, dh: u32) -> (f64, f64, f64, f64) {
    let (sw, sh) = (sw as f64, sh as f64);
    let target = dw as f64 / dh as f64;
    let (cw, ch) = if sw / sh > target {
        (sh * target, sh)
    } else {
        (sw, sw / target)
    };
    ((sw - cw) / 2.0, (sh - ch) / 2.0, cw, ch)
}

/// Scale `src` to fill `dw` x `dh` exactly, cropping its centre to the
/// target's aspect ratio (CSS `object-fit: cover`). Alpha is plain data.
pub(crate) fn resize_cover_into(r: &mut Resizer, src: &Frame, dst: &mut [u8], dw: u32, dh: u32) {
    let (left, top, cw, ch) = cover_crop(src.width, src.height, dw, dh);
    let src_img = ImageRef::new(src.width, src.height, &src.data, PixelType::U8x4)
        .expect("Frame invariant: len == w*h*4");
    let mut dst_img =
        Image::from_slice_u8(dw, dh, dst, PixelType::U8x4).expect("buffer sized by caller");
    let opts = bilinear().use_alpha(false).crop(left, top, cw, ch);
    r.resize(&src_img, &mut dst_img, &opts)
        .expect("same pixel type on both sides");
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
pub(crate) fn resize_into(
    r: &mut Resizer,
    src: &Frame,
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    use_alpha: bool,
) {
    let src_img = ImageRef::new(src.width, src.height, &src.data, PixelType::U8x4)
        .expect("Frame invariant: len == w*h*4");
    let mut dst_img =
        Image::from_slice_u8(dw, dh, dst, PixelType::U8x4).expect("buffer sized by caller");
    r.resize(&src_img, &mut dst_img, &bilinear().use_alpha(use_alpha))
        .expect("same pixel type on both sides");
}

/// Scale a one-byte-per-pixel mask into `dst` (`dw * dh` bytes).
fn scale_mask_into(r: &mut Resizer, m: &Mask, dst: &mut [u8], dw: u32, dh: u32) {
    scale_u8_into(r, &m.data, m.width, m.height, dst, dw, dh);
}

/// Scale a one-byte-per-pixel `sw` x `sh` image into `dst` (`dw * dh` bytes).
fn scale_u8_into(r: &mut Resizer, src: &[u8], sw: u32, sh: u32, dst: &mut [u8], dw: u32, dh: u32) {
    let src_img =
        ImageRef::new(sw, sh, src, PixelType::U8).expect("one byte per pixel: len == w*h");
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
        Compositor::new().compose(page, pip, &Backdrop::default(), out);
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
            &Backdrop::default(),
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
            &Backdrop::default(),
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
        assert!(c.compose(Some(&page), None, &Backdrop::default(), &mut out));
        assert!(
            !c.compose(Some(&page), None, &Backdrop::default(), &mut out),
            "same inputs redrawn"
        );
        assert_eq!(out.pixel(1, 1), RED);
        let page2 = solid(4, 4, BLUE);
        assert!(c.compose(Some(&page2), None, &Backdrop::default(), &mut out));
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
        assert!(c.compose(Some(&page2), pip(), &Backdrop::default(), &mut out));
        assert!(!c.compose(Some(&page2), pip(), &Backdrop::default(), &mut out));
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

    /// Compose the way the app did before backdrops existed.
    fn compose_today(
        c: &mut Compositor,
        page: Option<&Arc<Frame>>,
        pip: Option<Pip>,
        out: &mut Frame,
    ) -> bool {
        c.compose(page, pip, &Backdrop::default(), out)
    }

    fn fnv(data: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in data {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    /// Textured BGRA test picture (alpha varies too: it is not transparency).
    fn textured(w: u32, h: u32, seed: u32) -> Arc<Frame> {
        let mut d = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = (x * 7 + y * 13 + seed * 31) % 256;
                d.extend_from_slice(&[
                    v as u8,
                    (v * 3 % 256) as u8,
                    (255 - v) as u8,
                    (x % 256) as u8,
                ]);
            }
        }
        Arc::new(Frame::from_bgra(w, h, d).unwrap())
    }

    fn ramp_mask(w: u32, h: u32, edge: EdgeRange) -> Arc<Mask> {
        let data = (0..h)
            .flat_map(|y| (0..w).map(move |x| ((x * 255 / w.max(1) + y * 3) % 256) as u8))
            .collect();
        let mut m = Mask::new(w, h, data).unwrap();
        m.edge = edge;
        Arc::new(m)
    }

    /// The existing compose cases plus a few realistic ones; returns a hash
    /// of each output buffer.
    fn today_hashes() -> Vec<u64> {
        fn run_into(
            hashes: &mut Vec<u64>,
            page: Option<&Arc<Frame>>,
            pip: Option<Pip>,
            w: u32,
            h: u32,
        ) {
            let mut out = Frame::new_black(w, h);
            compose_today(&mut Compositor::new(), page, pip, &mut out);
            hashes.push(fnv(&out.data));
        }
        let mut hashes = Vec::new();
        macro_rules! run {
            ($($a:expr),*) => { run_into(&mut hashes, $($a),*) };
        }
        let box_tl = crate::PipLayout {
            visible: true,
            x: 0.0,
            y: 0.0,
            width: 0.25,
            style: OverlayStyle::Box,
        };
        let blue16 = solid(16, 9, BLUE);
        fn pip<'a>(
            cam: &'a Arc<Frame>,
            mask: Option<&'a Arc<Mask>>,
            layout: &'a crate::PipLayout,
        ) -> Pip<'a> {
            Pip { cam, mask, layout }
        }
        run!(
            Some(&solid(64, 36, RED)),
            Some(pip(&blue16, None, &box_tl)),
            64,
            36
        );
        let black3 = solid(3, 1, BLACK);
        let m3 = mask(3, 1, vec![0, 128, 255]);
        let full = full_layout(OverlayStyle::CutoutFull);
        run!(
            Some(&solid(3, 1, WHITE)),
            Some(pip(&black3, Some(&m3), &full)),
            3,
            1
        );
        let black16 = solid(16, 9, BLACK);
        let m2 = mask(2, 1, vec![0, 255]);
        run!(
            Some(&solid(64, 36, WHITE)),
            Some(pip(&black16, Some(&m2), &full)),
            64,
            36
        );
        let mut me = Mask::new(1, 1, vec![100]).unwrap();
        me.edge = EdgeRange { lo: 64, hi: 192 };
        let me = Arc::new(me);
        let black1 = solid(1, 1, BLACK);
        run!(
            Some(&solid(1, 1, WHITE)),
            Some(pip(&black1, Some(&me), &full)),
            1,
            1
        );
        let clear = Arc::new(Frame::from_bgra(3, 1, [255, 0, 0, 0].repeat(3)).unwrap());
        run!(
            Some(&solid(3, 1, WHITE)),
            Some(pip(&clear, None, &full)),
            3,
            1
        );
        let mut boxl = full_layout(OverlayStyle::Box);
        boxl.x = 0.0;
        let m0 = mask(1, 1, vec![0]);
        run!(
            Some(&solid(3, 1, WHITE)),
            Some(pip(&black3, Some(&m0), &boxl)),
            3,
            1
        );
        let mut left = full_layout(OverlayStyle::CutoutFull);
        left.x = 0.0;
        let m1 = mask(1, 1, vec![255]);
        let t16 = textured(16, 9, 2);
        run!(
            Some(&textured(64, 36, 1)),
            Some(pip(&t16, Some(&m1), &left)),
            64,
            36
        );
        run!(None, None, 8, 4);
        run!(Some(&solid(2, 2, RED)), None, 8, 4);
        // Realistic: textured page scaled, textured cam, ramp mask with an edge range.
        let page = textured(200, 100, 3);
        let cam = textured(160, 90, 4);
        let rm = ramp_mask(32, 18, EdgeRange { lo: 40, hi: 200 });
        let rm_full = ramp_mask(32, 18, EdgeRange::FULL);
        for style in [
            OverlayStyle::Box,
            OverlayStyle::CutoutSmall,
            OverlayStyle::CutoutFull,
        ] {
            for (x, y, width) in [(0.0, 0.0, 0.3), (0.7, 1.0, 0.5), (1.0, 0.5, 1.0)] {
                let l = crate::PipLayout {
                    visible: true,
                    x,
                    y,
                    width,
                    style,
                };
                for m in [None, Some(&rm), Some(&rm_full)] {
                    run!(Some(&page), Some(pip(&cam, m, &l)), 128, 72);
                }
            }
        }
        // One compositor over a sequence (cached layers).
        let mut c = Compositor::new();
        let mut out = Frame::new_black(128, 72);
        let l = full_layout(OverlayStyle::CutoutSmall);
        for i in 0..4 {
            let cam = textured(160, 90, 10 + i);
            let m = if i == 2 { None } else { Some(&rm) };
            compose_today(&mut c, Some(&page), Some(pip(&cam, m, &l)), &mut out);
            hashes.push(fnv(&out.data));
        }
        hashes
    }

    /// Captured from the compositor before backdrops existed (commit 8ba83cc).
    const TODAY: [u64; 40] = [
        17087990969124566949,
        11452694550105716032,
        14346249456586019877,
        12615725995947343858,
        4576583170131370447,
        4196988945988689160,
        12279062693804911333,
        17872749636569778213,
        4914351518924297989,
        4412669781667492230,
        4412669781667492230,
        4412669781667492230,
        12183725017727945837,
        12183725017727945837,
        12183725017727945837,
        18075435999054415292,
        18075435999054415292,
        18075435999054415292,
        4412669781667492230,
        14940941539700980407,
        4683462919711548773,
        12183725017727945837,
        7382225711564276180,
        9515823464186355170,
        18075435999054415292,
        18280677071460770124,
        13295847053090660265,
        4412669781667492230,
        15096704085669184986,
        964838965841836318,
        12183725017727945837,
        5871353251592598426,
        5421969731765486812,
        18075435999054415292,
        3207494351495630113,
        5703717235816604510,
        18370368021677169325,
        4789347542786211331,
        18085275646141487779,
        5082153273345802793,
    ];

    #[test]
    fn refined_off_is_bit_identical_to_today() {
        assert_eq!(today_hashes(), TODAY);
    }

    // --- Backdrops ---

    const MAGENTA: [u8; 4] = [255, 0, 255, 255];
    /// Pale yellow: much brighter than the room, so a luma guide can tell them apart.
    const PERSON: [u8; 4] = [180, 255, 255, 255];

    /// `w` x `h` webcam: left half person (solid `PERSON`), right half room (a 2 px
    /// green/blue checker, so a sharp room is easy to tell from a blurred one).
    fn person_cam(w: u32, h: u32) -> Arc<Frame> {
        let mut d = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let px = if x < w / 2 {
                    PERSON
                } else if (x / 2 + y / 2) % 2 == 0 {
                    GREEN
                } else {
                    BLUE
                };
                d.extend_from_slice(&px);
            }
        }
        Arc::new(Frame::from_bgra(w, h, d).unwrap())
    }

    /// Person mask matching `person_cam` (left half 255), model size.
    fn person_mask() -> Arc<Mask> {
        let data = (0..36)
            .flat_map(|_| (0..64).map(|x| if x < 32 { 255 } else { 0 }))
            .collect();
        mask(64, 36, data)
    }

    /// A sharp room pixel: pure green or pure blue from the checker.
    fn sharp_room(p: [u8; 4]) -> bool {
        p[2] < 40 && ((p[1] > 215 && p[0] < 40) || (p[0] > 215 && p[1] < 40))
    }

    /// A blurred room pixel: the checker mixed (green and blue both mid).
    fn blurred_room(p: [u8; 4]) -> bool {
        p[2] < 60 && (60..=200).contains(&p[1]) && (60..=200).contains(&p[0])
    }

    fn is_person(p: [u8; 4]) -> bool {
        p[2] > 230 && p[1] > 230 && (150..=210).contains(&p[0])
    }

    fn backdrop(full: FullBackdrop, in_box: BoxBackdrop) -> Backdrop {
        Backdrop {
            full,
            in_box,
            refined_edges: false,
        }
    }

    /// Compose once at 320x180 over a white page; returns the frame and the note.
    fn draw(
        cam: &Arc<Frame>,
        m: Option<&Arc<Mask>>,
        layout: &crate::PipLayout,
        b: &Backdrop,
    ) -> (Frame, ComposeNote) {
        let mut out = Frame::new_black(320, 180);
        let mut c = Compositor::new();
        let pip = Pip {
            cam,
            mask: m,
            layout,
        };
        assert!(c.compose(Some(&solid(320, 180, WHITE)), Some(pip), b, &mut out));
        (out, c.note())
    }

    #[test]
    fn backdrop_table() {
        let cam = person_cam(256, 144);
        let m = person_mask();
        let image = solid(7, 5, MAGENTA);
        let big_box = crate::PipLayout {
            visible: true,
            x: 0.0,
            y: 0.0,
            width: 0.8,
            style: OverlayStyle::Box,
        };
        let (bx, by, bw, bh) = pip_rect(&big_box, 256, 144, 320, 180).unwrap();
        let bx = bx as u32;
        let person = (bx + bw / 4, by + bh / 2);
        let room = (bx + bw * 3 / 4, by + bh / 2);
        let outside = (318, 178);
        use BoxBackdrop::*;
        type Check = fn([u8; 4]) -> bool;
        let is_white: Check = |p| p == WHITE;
        let is_magenta: Check = |p| p == MAGENTA;
        let is_colour: Check = |p| p == [30, 20, 10, 255];
        let sharp: Check = sharp_room;
        let blurred: Check = blurred_room;
        let red: Check = is_person;
        // (in_box, fresh mask, person probe, room probe, note)
        let rows: Vec<(BoxBackdrop, bool, Check, Check, ComposeNote)> = vec![
            (Room, true, red, sharp, ComposeNote::None),
            (Room, false, red, sharp, ComposeNote::None),
            (Blur, true, red, blurred, ComposeNote::None),
            (Blur, false, red, blurred, ComposeNote::CutoutNotReady),
            (
                Image(image.clone()),
                true,
                red,
                is_magenta,
                ComposeNote::None,
            ),
            (
                Image(image.clone()),
                false,
                red,
                sharp,
                ComposeNote::CutoutNotReady,
            ),
            (
                Colour([10, 20, 30]),
                true,
                red,
                is_colour,
                ComposeNote::None,
            ),
            (
                Colour([10, 20, 30]),
                false,
                red,
                sharp,
                ComposeNote::CutoutNotReady,
            ),
        ];
        for (in_box, fresh, want_person, want_room, want_note) in rows {
            let b = backdrop(FullBackdrop::Page, in_box.clone());
            let (out, note) = draw(&cam, fresh.then_some(&m), &big_box, &b);
            let label = format!("{in_box:?} fresh={fresh}");
            assert!(
                want_person(out.pixel(person.0, person.1)),
                "{label}: person {:?}",
                out.pixel(person.0, person.1)
            );
            assert!(
                want_room(out.pixel(room.0, room.1)),
                "{label}: room {:?}",
                out.pixel(room.0, room.1)
            );
            assert!(
                is_white(out.pixel(outside.0, outside.1)),
                "{label}: outside"
            );
            assert_eq!(note, want_note, "{label}");
            assert!(
                out.data.as_chunks::<4>().0.iter().all(|p| p[3] == 255),
                "{label}: opaque"
            );
            if matches!(in_box, Blur) {
                // Never the sharp room under a blur setting.
                for y in by..by + bh {
                    for x in bx..bx + bw {
                        assert!(!sharp_room(out.pixel(x, y)), "{label}: sharp at {x},{y}");
                    }
                }
            }
        }

        // Blurred room: a small cut-out; outside it the blurred webcam, not the page.
        let small = crate::PipLayout {
            visible: true,
            x: 1.0,
            y: 0.0,
            width: 0.5,
            style: OverlayStyle::CutoutSmall,
        };
        let cam = person_cam(320, 180);
        let (sx, sy, sw, sh) = pip_rect(&small, 320, 180, 320, 180).unwrap();
        let sx = sx as u32;
        let b = backdrop(FullBackdrop::BlurredRoom, Room);
        let (out, note) = draw(&cam, Some(&m), &small, &b);
        assert_eq!(note, ComposeNote::None);
        assert!(
            is_person(out.pixel(sx + sw / 4, sy + sh / 2)),
            "person drawn"
        );
        assert!(
            blurred_room(out.pixel(sx + sw * 3 / 4, sy + sh / 2)),
            "cut-out room shows the blur"
        );
        assert!(
            blurred_room(out.pixel(300, 20)),
            "frame room is blurred: {:?}",
            out.pixel(300, 20)
        );
        assert!(
            is_person(out.pixel(20, 90)),
            "blurred person half of the frame"
        );
        assert!(!out.data.as_chunks::<4>().0.contains(&WHITE), "page shown");
        // Stale mask: the whole frame blurred, person too, plus the note.
        let (out, note) = draw(&cam, None, &small, &b);
        assert_eq!(note, ComposeNote::CutoutNotReady);
        assert!(blurred_room(out.pixel(sx + sw * 3 / 4, sy + sh / 2)));
        assert!(blurred_room(out.pixel(300, 20)));
        let px = out.data.as_chunks::<4>().0;
        assert!(!px.iter().any(|p| *p == WHITE || sharp_room(*p)));
        // Blurred near the person/room seam (x = 160): no hard step.
        let step = out.pixel(158, 90)[2].abs_diff(out.pixel(162, 90)[2]);
        assert!(step < 120, "seam not blurred: step {step}");
    }

    #[test]
    fn blurred_room_never_shows_sharp_room() {
        let cam = person_cam(320, 180);
        let m = person_mask();
        for style in [
            OverlayStyle::Box,
            OverlayStyle::CutoutSmall,
            OverlayStyle::CutoutFull,
        ] {
            for (x, width) in [(0.0, 0.3), (0.5, 1.0), (1.0, 0.9)] {
                let l = crate::PipLayout {
                    visible: true,
                    x,
                    y: 1.0,
                    width,
                    style,
                };
                for fresh in [true, false] {
                    for refined in [false, true] {
                        // In-box Room too: the app forbids Box under a blurred
                        // room, but layout and backdrop are read separately.
                        for in_box in [BoxBackdrop::Blur, BoxBackdrop::Room] {
                            let b = Backdrop {
                                full: FullBackdrop::BlurredRoom,
                                in_box: in_box.clone(),
                                refined_edges: refined,
                            };
                            let (out, _) = draw(&cam, fresh.then_some(&m), &l, &b);
                            let sharp = out
                                .data
                                .as_chunks::<4>()
                                .0
                                .iter()
                                .filter(|p| sharp_room(**p))
                                .count();
                            assert_eq!(
                                sharp, 0,
                                "{style:?} x={x} fresh={fresh} refined={refined} {in_box:?}"
                            );
                            assert!(out.data.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
                        }
                    }
                }
            }
        }
        // A Box drawn 1:1 (so a sharp room would be detected exactly), in-box
        // Room, fresh mask: the person over the blurred room.
        let cam = person_cam(256, 144);
        let l = crate::PipLayout {
            visible: true,
            x: 0.0,
            y: 0.0,
            width: 0.8,
            style: OverlayStyle::Box,
        };
        let (bx, by, bw, bh) = pip_rect(&l, 256, 144, 320, 180).unwrap();
        assert_eq!((bw, bh), (256, 144));
        for in_box in [BoxBackdrop::Room, BoxBackdrop::Blur] {
            let b = backdrop(FullBackdrop::BlurredRoom, in_box.clone());
            let (out, _) = draw(&cam, Some(&m), &l, &b);
            let bx = bx as u32;
            assert!(is_person(out.pixel(bx + bw / 4, by + bh / 2)), "{in_box:?}");
            assert!(
                blurred_room(out.pixel(bx + bw * 3 / 4, by + bh / 2)),
                "{in_box:?}: {:?}",
                out.pixel(bx + bw * 3 / 4, by + bh / 2)
            );
            assert!(!out.data.as_chunks::<4>().0.iter().any(|p| sharp_room(*p)));
        }
        // No webcam at all: nothing to blur; the page shows, with a note.
        let mut out = Frame::new_black(32, 18);
        let mut c = Compositor::new();
        c.compose(
            Some(&solid(32, 18, WHITE)),
            None,
            &backdrop(FullBackdrop::BlurredRoom, BoxBackdrop::Room),
            &mut out,
        );
        assert!(out.data.as_chunks::<4>().0.iter().all(|p| *p == WHITE));
        assert_eq!(c.note(), ComposeNote::WebcamOff);
    }

    #[test]
    fn blurred_room_is_cover_cropped() {
        // A 4:3 webcam on a 16:9 output: the top and bottom 1/8 are cropped
        // away (cover), not squeezed in. Those bands are the person colour.
        let (w, h) = (160u32, 120u32);
        let mut d = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let px = if y < 15 || y >= h - 15 {
                    PERSON
                } else if (x / 2 + y / 2) % 2 == 0 {
                    GREEN
                } else {
                    BLUE
                };
                d.extend_from_slice(&px);
            }
        }
        let cam = Arc::new(Frame::from_bgra(w, h, d).unwrap());
        let hidden = crate::PipLayout {
            visible: false,
            ..crate::PipLayout::default()
        };
        let b = backdrop(FullBackdrop::BlurredRoom, BoxBackdrop::Room);
        let (out, note) = draw(&cam, None, &hidden, &b);
        assert_eq!(note, ComposeNote::None);
        for (x, y) in [(160, 12), (160, 90), (40, 168)] {
            assert!(
                blurred_room(out.pixel(x, y)),
                "({x},{y}) {:?}",
                out.pixel(x, y)
            );
        }
    }

    #[test]
    fn cutout_styles_ignore_box_backdrop() {
        let cam = person_cam(160, 90);
        let m = person_mask();
        for style in [OverlayStyle::CutoutSmall, OverlayStyle::CutoutFull] {
            let l = crate::PipLayout {
                visible: true,
                x: 0.3,
                y: 0.0,
                width: 0.5,
                style,
            };
            for fresh in [true, false] {
                let (want, _) = draw(&cam, fresh.then_some(&m), &l, &Backdrop::default());
                for in_box in [
                    BoxBackdrop::Blur,
                    BoxBackdrop::Image(solid(4, 4, MAGENTA)),
                    BoxBackdrop::Colour([1, 2, 3]),
                ] {
                    let (got, note) = draw(
                        &cam,
                        fresh.then_some(&m),
                        &l,
                        &backdrop(FullBackdrop::Page, in_box.clone()),
                    );
                    assert!(got == want, "{style:?} fresh={fresh} {in_box:?}");
                    assert_eq!(note, ComposeNote::None);
                }
            }
        }
    }

    #[test]
    fn refined_edges_skip_the_edge_lut() {
        // Flat mask 100 with a 64..192 edge range: the LUT makes it mostly
        // room; the guided filter keeps ~100 (it replaces the LUT).
        let cam = solid(64, 36, BLACK);
        let mut m = Mask::new(16, 9, vec![100; 144]).unwrap();
        m.edge = EdgeRange { lo: 64, hi: 192 };
        let m = Arc::new(m);
        let l = full_layout(OverlayStyle::CutoutFull);
        let mut b = Backdrop::default();
        let (off, _) = draw(&cam, Some(&m), &l, &b);
        b.refined_edges = true;
        let (on, _) = draw(&cam, Some(&m), &l, &b);
        let (v_off, v_on) = (off.pixel(160, 90)[0], on.pixel(160, 90)[0]);
        assert!(v_off > 200, "LUT path {v_off}");
        assert!(v_on.abs_diff(155) <= 3, "guided path {v_on}");
    }

    #[test]
    fn refined_matte_time_is_recorded_for_the_stats_line() {
        let cam = person_cam(160, 90);
        let m = person_mask();
        let l = full_layout(OverlayStyle::CutoutFull);
        let mut b = Backdrop::default();
        let mut c = Compositor::new();
        let mut out = Frame::new_black(320, 180);
        let page = solid(320, 180, WHITE);
        let pip = || Pip {
            cam: &cam,
            mask: Some(&m),
            layout: &l,
        };
        assert!(c.compose(Some(&page), Some(pip()), &b, &mut out));
        assert_eq!(c.take_refined_summary(), None, "refined edges off");
        b.refined_edges = true;
        assert!(c.compose(Some(&page), Some(pip()), &b, &mut out));
        assert_eq!(c.take_refined_summary().map(|s| s.count), Some(1));
        assert_eq!(c.take_refined_summary(), None, "a new window after taking");
    }

    #[test]
    fn refined_edges_follow_the_person_at_any_pip_size() {
        // Includes a PIP wider than 960 px (guided filter capped, then upscaled).
        for (ow, oh) in [(320u32, 180u32), (1920, 1080)] {
            let cam = person_cam(160, 90);
            let m = person_mask();
            let l = full_layout(OverlayStyle::CutoutFull);
            let b = Backdrop {
                refined_edges: true,
                ..Backdrop::default()
            };
            let mut out = Frame::new_black(ow, oh);
            let mut c = Compositor::new();
            let pip = Pip {
                cam: &cam,
                mask: Some(&m),
                layout: &l,
            };
            c.compose(Some(&solid(ow, oh, WHITE)), Some(pip), &b, &mut out);
            assert!(is_person(out.pixel(ow / 4, oh / 2)), "{ow}: person");
            assert_eq!(out.pixel(ow * 7 / 8, oh / 2), WHITE, "{ow}: room cut away");
        }
    }

    #[test]
    fn backdrop_changes_and_new_frames_redraw() {
        let mut c = Compositor::new();
        let mut out = Frame::new_black(64, 36);
        let page = solid(64, 36, WHITE);
        let cam = person_cam(160, 90);
        let m = person_mask();
        let l = full_layout(OverlayStyle::Box);
        let pip = |cam| {
            Some(Pip {
                cam,
                mask: Some(&m),
                layout: &l,
            })
        };
        let mut b = backdrop(FullBackdrop::Page, BoxBackdrop::Blur);
        assert!(c.compose(Some(&page), pip(&cam), &b, &mut out));
        assert!(!c.compose(Some(&page), pip(&cam), &b, &mut out));
        assert!(c.is_unchanged(Some(&page), pip(&cam), &b, 64, 36));
        // A new webcam frame rebuilds the blur.
        let cam2 = person_cam(160, 90);
        assert!(c.compose(Some(&page), pip(&cam2), &b, &mut out));
        let img = solid(4, 4, MAGENTA);
        b.in_box = BoxBackdrop::Image(img.clone());
        assert!(c.compose(Some(&page), pip(&cam2), &b, &mut out));
        assert!(!c.compose(Some(&page), pip(&cam2), &b, &mut out));
        // Another image (same content, new Arc) redraws.
        b.in_box = BoxBackdrop::Image(solid(4, 4, MAGENTA));
        assert!(c.compose(Some(&page), pip(&cam2), &b, &mut out));
        b.in_box = BoxBackdrop::Colour([1, 2, 3]);
        assert!(c.compose(Some(&page), pip(&cam2), &b, &mut out));
        b.refined_edges = true;
        assert!(c.compose(Some(&page), pip(&cam2), &b, &mut out));
        b.full = FullBackdrop::BlurredRoom;
        assert!(c.compose(Some(&page), pip(&cam2), &b, &mut out));
        assert!(!c.compose(Some(&page), pip(&cam2), &b, &mut out));
        // Under a blurred room, page repaints don't matter.
        assert!(!c.compose(Some(&solid(64, 36, RED)), pip(&cam2), &b, &mut out));
    }

    #[test]
    fn image_backdrop_is_cover_scaled() {
        // A 4:1 image (left quarter red, rest yellow) covering a 16:9 box: the
        // sides are cropped, so the red quarter is cut off entirely.
        let mut d = Vec::new();
        for _ in 0..10 {
            for x in 0..40 {
                d.extend_from_slice(if x < 10 { &RED } else { &MAGENTA });
            }
        }
        let img = Arc::new(Frame::from_bgra(40, 10, d).unwrap());
        let cam = person_cam(160, 90);
        let empty = mask(4, 4, vec![0; 16]);
        let l = crate::PipLayout {
            visible: true,
            x: 0.0,
            y: 0.0,
            width: 0.5,
            style: OverlayStyle::Box,
        };
        let (bx, by, bw, bh) = pip_rect(&l, 160, 90, 320, 180).unwrap();
        let (out, _) = draw(
            &cam,
            Some(&empty),
            &l,
            &backdrop(FullBackdrop::Page, BoxBackdrop::Image(img)),
        );
        for x in [bx as u32 + 1, bx as u32 + bw / 2, bx as u32 + bw - 2] {
            assert_eq!(out.pixel(x, by + bh / 2), MAGENTA, "x={x}");
        }
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
