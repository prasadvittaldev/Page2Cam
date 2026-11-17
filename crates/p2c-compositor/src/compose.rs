use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use p2c_core::Frame;

/// Draw `page` into `out`. With no page, `out` becomes black. When the
/// sizes match, the page is copied. Otherwise it is scaled to fit, keeping
/// its aspect ratio, centered with black bars.
pub fn compose(page: Option<&Frame>, out: &mut Frame) {
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
    let scaled = scale(page, dw, dh);
    blit(scaled.buffer(), dw, dh, out, dx, dy);
}

/// Largest rect with the source aspect ratio that fits in the output, centered.
fn fit_rect(sw: u32, sh: u32, ow: u32, oh: u32) -> (u32, u32, u32, u32) {
    let scale = (ow as f64 / sw as f64).min(oh as f64 / sh as f64);
    let dw = ((sw as f64 * scale).round() as u32).min(ow);
    let dh = ((sh as f64 * scale).round() as u32).min(oh);
    ((ow - dw) / 2, (oh - dh) / 2, dw, dh)
}

pub(crate) fn scale(src: &Frame, dw: u32, dh: u32) -> Image<'static> {
    let src_img = ImageRef::new(src.width, src.height, &src.data, PixelType::U8x4)
        .expect("Frame invariant: len == w*h*4");
    let mut dst = Image::new(dw, dh, PixelType::U8x4);
    let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear));
    Resizer::new()
        .resize(&src_img, &mut dst, &opts)
        .expect("same pixel type on both sides");
    dst
}

/// Copy a tightly packed BGRA `src` (w x h) into `out` at (x, y). Caller guarantees it fits.
pub(crate) fn blit(src: &[u8], w: u32, h: u32, out: &mut Frame, x: u32, y: u32) {
    let row = w as usize * 4;
    let ostride = out.width as usize * 4;
    for r in 0..h as usize {
        let o = (y as usize + r) * ostride + x as usize * 4;
        out.data[o..o + row].copy_from_slice(&src[r * row..(r + 1) * row]);
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
    use p2c_core::Frame;

    const RED: [u8; 4] = [0, 0, 255, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Frame {
        Frame::from_bgra(w, h, px.repeat((w * h) as usize)).unwrap()
    }

    #[test]
    fn tick_without_page_emits_black() {
        let mut out = solid(8, 4, RED);
        compose(None, &mut out);
        for (x, y) in [(0, 0), (4, 2), (7, 3)] {
            assert_eq!(out.pixel(x, y), BLACK);
        }
    }

    #[test]
    fn compose_same_size_copies() {
        let mut out = Frame::new_black(4, 4);
        compose(Some(&solid(4, 4, RED)), &mut out);
        assert_eq!(out.pixel(2, 2), RED);
    }

    #[test]
    fn tick_scales_mismatched_page() {
        let mut out = Frame::new_black(8, 4);
        compose(Some(&solid(2, 2, RED)), &mut out);
        assert_eq!((out.width, out.height, out.data.len()), (8, 4, 128));
        assert_eq!(out.pixel(4, 2), RED);
        assert_eq!(out.pixel(0, 2), BLACK);
    }
}
