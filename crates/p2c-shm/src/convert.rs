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

/// Bilinear resample of one plane with `ch` interleaved channels (1 = Y, 2 = UV).
#[allow(clippy::too_many_arguments)]
fn scale_plane(
    src: &[u8],
    sstride: usize,
    sw: usize,
    sh: usize,
    dst: &mut [u8],
    dstride: usize,
    dw: usize,
    dh: usize,
    ch: usize,
) {
    // Fixed-point (16.16) sampling at pixel centres.
    let xs: Vec<(usize, usize, u32)> = (0..dw)
        .map(|x| {
            let f = ((2 * x + 1) * sw * 65536 / (2 * dw)).saturating_sub(32768);
            let x0 = (f >> 16).min(sw - 1);
            (x0, (x0 + 1).min(sw - 1), (f & 0xFFFF) as u32)
        })
        .collect();
    for y in 0..dh {
        let f = ((2 * y + 1) * sh * 65536 / (2 * dh)).saturating_sub(32768);
        let y0 = (f >> 16).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let wy = (f & 0xFFFF) as u32;
        let (r0, r1) = (&src[y0 * sstride..], &src[y1 * sstride..]);
        let drow = &mut dst[y * dstride..y * dstride + dw * ch];
        for (x, &(x0, x1, wx)) in xs.iter().enumerate() {
            for c in 0..ch {
                let top = r0[x0 * ch + c] as u32 * (65536 - wx) + r0[x1 * ch + c] as u32 * wx;
                let bot = r1[x0 * ch + c] as u32 * (65536 - wx) + r1[x1 * ch + c] as u32 * wx;
                let v = ((top as u64 * (65536 - wy) as u64 + bot as u64 * wy as u64) >> 32) as u32;
                drow[x * ch + c] = v as u8;
            }
        }
    }
}

/// Copy a tightly packed NV12 frame (`sw`x`sh`) into `dst` (`dw`x`dh`, row
/// stride `pitch`, UV plane at `pitch * dh`). Same size: row copy; otherwise
/// bilinear per plane. Sizes must be even.
pub fn nv12_to_nv12(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], pitch: usize, dw: u32, dh: u32) {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    if sw < 2
        || sh < 2
        || dw < 2
        || dh < 2
        || !sw.is_multiple_of(2)
        || !sh.is_multiple_of(2)
        || !dw.is_multiple_of(2)
        || !dh.is_multiple_of(2)
        || pitch < dw
        || src.len() < sw * sh * 3 / 2
        || dst.len() < pitch * dh * 3 / 2
    {
        return;
    }
    let (suv, duv) = (sw * sh, pitch * dh);
    if sw == dw && sh == dh {
        for y in 0..dh {
            dst[y * pitch..y * pitch + dw].copy_from_slice(&src[y * sw..y * sw + sw]);
        }
        for y in 0..dh / 2 {
            dst[duv + y * pitch..duv + y * pitch + dw]
                .copy_from_slice(&src[suv + y * sw..suv + y * sw + sw]);
        }
        return;
    }
    scale_plane(src, sw, sw, sh, dst, pitch, dw, dh, 1);
    scale_plane(
        &src[suv..],
        sw,
        sw / 2,
        sh / 2,
        &mut dst[duv..],
        pitch,
        dw / 2,
        dh / 2,
        2,
    );
}

/// NV12 (BT.709 limited range) to RGB32 (BGRA in memory, full range, opaque)
/// with nearest-neighbour scaling and row stride `pitch`.
pub fn nv12_to_rgb32(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], pitch: usize, dw: u32, dh: u32) {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    if sw < 2
        || sh < 2
        || dw == 0
        || dh == 0
        || !sw.is_multiple_of(2)
        || !sh.is_multiple_of(2)
        || src.len() < sw * sh * 3 / 2
        || dst.len() < pitch * (dh - 1) + dw * 4
    {
        return;
    }
    let uv = sw * sh;
    for y in 0..dh {
        let sy = y * sh / dh;
        for x in 0..dw {
            let sx = x * sw / dw;
            let c = src[sy * sw + sx] as i32 - 16;
            let ui = uv + (sy / 2) * sw + (sx / 2) * 2;
            let d = src[ui] as i32 - 128;
            let e = src[ui + 1] as i32 - 128;
            let c = 298 * c + 128;
            let o = y * pitch + x * 4;
            dst[o] = ((c + 541 * d) >> 8).clamp(0, 255) as u8;
            dst[o + 1] = ((c - 55 * d - 136 * e) >> 8).clamp(0, 255) as u8;
            dst[o + 2] = ((c + 459 * e) >> 8).clamp(0, 255) as u8;
            dst[o + 3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, bgra: [u8; 4]) -> Vec<u8> {
        bgra.repeat((w * h) as usize)
    }

    /// Per-frame cost of the conversions the camera DLL runs inside Frame Server.
    /// `cargo test --release -p p2c-shm bench_dll_conversions -- --ignored --nocapture`
    #[test]
    #[ignore]
    #[allow(clippy::type_complexity)]
    fn bench_dll_conversions() {
        let (sw, sh) = (1920u32, 1080u32);
        let src: Vec<u8> = (0..(sw * sh * 3 / 2) as usize)
            .map(|i| (i * 31 % 251) as u8)
            .collect();
        let run =
            |name: &str, f: &dyn Fn(&mut [u8], usize, u32, u32), dw: u32, dh: u32, bpp: usize| {
                let pitch = dw as usize * bpp;
                let rows = if bpp == 1 { dh + dh / 2 } else { dh } as usize;
                let mut dst = vec![0u8; pitch * rows];
                let mut t: Vec<f64> = (0..300)
                    .map(|_| {
                        let s = std::time::Instant::now();
                        f(&mut dst, pitch, dw, dh);
                        std::hint::black_box(&dst);
                        s.elapsed().as_secs_f64() * 1000.0
                    })
                    .collect();
                t.sort_by(|a, b| a.partial_cmp(b).unwrap());
                println!(
                    "{name}: median {:.2} ms  p95 {:.2} ms  max {:.2} ms",
                    t[150], t[284], t[299]
                );
            };
        run(
            "nv12 1080p->720p scale",
            &|d, p, w, h| nv12_to_nv12(&src, sw, sh, d, p, w, h),
            1280,
            720,
            1,
        );
        run(
            "nv12 1080p->1080p copy",
            &|d, p, w, h| nv12_to_nv12(&src, sw, sh, d, p, w, h),
            1920,
            1080,
            1,
        );
        run(
            "nv12->rgb32 1080p",
            &|d, p, w, h| nv12_to_rgb32(&src, sw, sh, d, p, w, h),
            1920,
            1080,
            4,
        );
        run(
            "nv12->rgb32 1080p->720p",
            &|d, p, w, h| nv12_to_rgb32(&src, sw, sh, d, p, w, h),
            1280,
            720,
            4,
        );
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

    #[test]
    fn nv12_copy_respects_pitch() {
        let src: Vec<u8> = (1..=12).collect();
        let mut dst = vec![0xAAu8; 6 * 2 * 3 / 2];
        nv12_to_nv12(&src, 4, 2, &mut dst, 6, 4, 2);
        assert_eq!(&dst[0..4], &src[0..4]);
        assert_eq!(&dst[4..6], &[0xAA, 0xAA], "padding untouched");
        assert_eq!(&dst[6..10], &src[4..8]);
        assert_eq!(&dst[12..16], &src[8..12], "UV plane at pitch*dh");
        assert_eq!(&dst[16..18], &[0xAA, 0xAA]);
    }

    #[test]
    fn nv12_scale_1080_to_720_keeps_colour() {
        let (sw, sh, dw, dh) = (1920usize, 1080usize, 1280usize, 720usize);
        let mut src = vec![81u8; sw * sh];
        for _ in 0..sw * sh / 4 {
            src.extend_from_slice(&[90, 240]);
        }
        let mut dst = vec![0u8; dw * dh * 3 / 2];
        nv12_to_nv12(
            &src, sw as u32, sh as u32, &mut dst, dw, dw as u32, dh as u32,
        );
        assert!(dst[..dw * dh].iter().all(|b| (*b as i32 - 81).abs() <= 1));
        for uv in dst[dw * dh..].chunks(2) {
            assert!((uv[0] as i32 - 90).abs() <= 1 && (uv[1] as i32 - 240).abs() <= 1);
        }
    }

    #[test]
    fn nv12_to_rgb32_bt709() {
        for (y, u, v, want, tol) in [
            (235u8, 128u8, 128u8, [255i32, 255, 255, 255], 2),
            (63, 102, 240, [0, 0, 255, 255], 3),
        ] {
            let mut src = vec![y; 4 * 2];
            src.extend_from_slice(&[u, v, u, v]);
            let mut dst = vec![0u8; 4 * 2 * 4];
            nv12_to_rgb32(&src, 4, 2, &mut dst, 16, 4, 2);
            for px in dst.chunks(4) {
                for k in 0..4 {
                    assert!((px[k] as i32 - want[k]).abs() <= tol, "{px:?} vs {want:?}");
                }
            }
        }
    }
}
