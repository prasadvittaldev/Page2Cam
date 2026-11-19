//! Tray icon pictures: the app icon (embedded `page2cam.ico`) with a small
//! coloured health dot in the bottom-right corner, drawn at run time.

use crate::health::Level;

/// The exe's icon (same file `page2cam.rc` embeds).
const APP_ICO: &[u8] = include_bytes!("../../page2cam/windows/page2cam.ico");

/// Dot colour (RGB) per health level.
pub fn dot_colour(level: Level) -> [u8; 3] {
    match level {
        Level::Grey => [0x8a, 0x8f, 0x98],
        Level::Green => [0x22, 0xc5, 0x5e],
        Level::Amber => [0xf5, 0x9e, 0x0b],
        Level::Red => [0xef, 0x44, 0x44],
    }
}

/// The app icon scaled to `size` x `size`, RGBA (straight alpha).
pub fn base_rgba(size: u32) -> Vec<u8> {
    // The ICO decoder picks the largest entry; scale it down once per size.
    match image::load_from_memory_with_format(APP_ICO, image::ImageFormat::Ico) {
        Ok(img) => image::imageops::resize(
            &img.to_rgba8(),
            size,
            size,
            image::imageops::FilterType::Lanczos3,
        )
        .into_raw(),
        Err(e) => {
            // Never expected (the file is embedded); a plain navy square then.
            tracing::warn!("cannot decode the app icon: {e}");
            [0x0d, 0x1b, 0x2a, 0xff].repeat((size * size) as usize)
        }
    }
}

/// Centre and radii (dot, dot + white ring) of the health dot.
fn dot_geometry(size: u32) -> (f32, f32, f32) {
    let s = size as f32;
    let r = (s * 0.22).max(2.5);
    let ring = (s / 16.0).max(1.0);
    let c = s - r - ring;
    (c, r, r + ring)
}

/// The tray picture for `level`: RGBA, `size` x `size`.
pub fn icon_rgba(level: Level, size: u32) -> Vec<u8> {
    let mut img = base_rgba(size);
    let (c, r, outer) = dot_geometry(size);
    let [dr, dg, db] = dot_colour(level);
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 + 0.5 - (c + 0.5), y as f32 + 0.5 - (c + 0.5));
            let d = (dx * dx + dy * dy).sqrt();
            let rgb = if d <= r {
                [dr, dg, db]
            } else if d <= outer {
                [0xff, 0xff, 0xff]
            } else {
                continue;
            };
            let i = ((y * size + x) * 4) as usize;
            img[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 0xff]);
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(img: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    #[test]
    fn icon_variants_have_dot_colours() {
        for size in [16u32, 20, 24, 32] {
            let base = base_rgba(size);
            assert_eq!(base.len(), (size * size * 4) as usize);
            assert!(
                base.chunks(4).any(|p| p[3] > 0),
                "{size}: the app icon is not empty"
            );
            let (c, _, _) = dot_geometry(size);
            let (cx, cy) = (c as u32, c as u32);
            let mut centres = Vec::new();
            for level in [Level::Grey, Level::Green, Level::Amber, Level::Red] {
                let img = icon_rgba(level, size);
                assert_eq!(img.len(), base.len());
                let [r, g, b] = dot_colour(level);
                assert_eq!(px(&img, size, cx, cy), [r, g, b, 255], "{size} {level:?}");
                // The rest of the icon is untouched (top-left quarter).
                for y in 0..size / 2 {
                    for x in 0..size / 2 {
                        assert_eq!(px(&img, size, x, y), px(&base, size, x, y));
                    }
                }
                centres.push(px(&img, size, cx, cy));
            }
            centres.dedup();
            assert_eq!(centres.len(), 4, "four distinct dot colours");
        }
    }
}
