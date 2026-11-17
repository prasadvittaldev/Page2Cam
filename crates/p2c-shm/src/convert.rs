//! BGRA → camera sample formats, with nearest-neighbour scaling and an
//! arbitrary destination pitch (as Media Foundation's Lock2D returns).

/// Fill an NV12 buffer: `dh` rows of Y, then `dh / 2` rows of interleaved UV,
/// both with row stride `pitch`. BT.601 limited range.
pub fn fill_nv12(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], pitch: usize, dw: u32, dh: u32) {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    if sw == 0
        || sh == 0
        || dw == 0
        || dh == 0
        || src.len() < sw * sh * 4
        || dst.len() < pitch * dh * 3 / 2
    {
        return;
    }
    let sx: Vec<usize> = (0..dw).map(|x| x * sw / dw).collect();
    let px = |x: usize, y: usize| {
        let i = ((y * sh / dh) * sw + sx[x]) * 4;
        (src[i + 2] as i32, src[i + 1] as i32, src[i] as i32)
    };
    for y in 0..dh {
        let row = &mut dst[y * pitch..y * pitch + dw];
        for (x, o) in row.iter_mut().enumerate() {
            let (r, g, b) = px(x, y);
            *o = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
        }
    }
    let uv = pitch * dh;
    for y in 0..dh / 2 {
        for x in 0..dw / 2 {
            let (r, g, b) = px(x * 2, y * 2);
            let o = uv + y * pitch + x * 2;
            dst[o] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
            dst[o + 1] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;
        }
    }
}

/// Fill an RGB32 (BGRA in memory) buffer with row stride `pitch`.
pub fn fill_rgb32(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], pitch: usize, dw: u32, dh: u32) {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    if sw == 0
        || sh == 0
        || dw == 0
        || dh == 0
        || src.len() < sw * sh * 4
        || dst.len() < pitch * (dh - 1) + dw * 4
    {
        return;
    }
    for y in 0..dh {
        let srow = (y * sh / dh) * sw;
        for x in 0..dw {
            let s = (srow + x * sw / dw) * 4;
            let d = y * pitch + x * 4;
            dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, bgra: [u8; 4]) -> Vec<u8> {
        bgra.repeat((w * h) as usize)
    }

    #[test]
    fn nv12_white_black_red() {
        for (px, y, u, v) in [
            ([255, 255, 255, 255], 235, 128, 128),
            ([0, 0, 0, 255], 16, 128, 128),
            ([0, 0, 255, 255], 81, 90, 240),
        ] {
            let src = solid(4, 2, px);
            let mut dst = vec![0u8; 4 * 2 * 3 / 2];
            fill_nv12(&src, 4, 2, &mut dst, 4, 4, 2);
            assert!(
                dst[..8].iter().all(|b| (*b as i32 - y).abs() <= 1),
                "{px:?} Y {:?}",
                &dst[..8]
            );
            assert!(
                (dst[8] as i32 - u).abs() <= 1 && (dst[9] as i32 - v).abs() <= 1,
                "{px:?} UV {:?}",
                &dst[8..]
            );
        }
    }

    #[test]
    fn nv12_respects_pitch_and_scales() {
        // 2x2 red source scaled to 4x2 destination with pitch 6.
        let src = solid(2, 2, [0, 0, 255, 255]);
        let mut dst = vec![0xAAu8; 6 * 2 * 3 / 2];
        fill_nv12(&src, 2, 2, &mut dst, 6, 4, 2);
        assert!((dst[0] as i32 - 81).abs() <= 1 && (dst[3] as i32 - 81).abs() <= 1);
        assert_eq!(dst[4], 0xAA, "padding bytes untouched");
        assert!(
            (dst[12] as i32 - 90).abs() <= 1,
            "UV plane starts at pitch*height"
        );
    }

    #[test]
    fn rgb32_copies_with_scaling() {
        let src = solid(2, 1, [1, 2, 3, 255]);
        let mut dst = vec![0u8; 4 * 4];
        fill_rgb32(&src, 2, 1, &mut dst, 16, 4, 1);
        assert_eq!(&dst[12..16], &[1, 2, 3, 255]);
    }
}
