//! Raw webcam buffers → BGRA, in pure Rust (no C JPEG library to cross-compile).
#![allow(clippy::chunks_exact_to_as_chunks)]
use p2c_core::Frame;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawFormat {
    Mjpeg,
    Yuyv,
    Nv12,
}

/// Decode one camera buffer. Errors (never panics) on short or corrupt data.
pub fn to_bgra(fmt: RawFormat, width: u32, height: u32, src: &[u8]) -> anyhow::Result<Frame> {
    let (w, h) = (width as usize, height as usize);
    match fmt {
        RawFormat::Mjpeg => {
            let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::BGRA);
            let mut dec = JpegDecoder::new_with_options(src, opts);
            let data = dec.decode().map_err(|e| anyhow::anyhow!("jpeg: {e:?}"))?;
            let (dw, dh) = dec
                .dimensions()
                .ok_or_else(|| anyhow::anyhow!("jpeg: no dimensions"))?;
            Ok(Frame::from_bgra(dw as u32, dh as u32, data)?)
        }
        RawFormat::Yuyv => {
            anyhow::ensure!(w % 2 == 0 && src.len() >= w * h * 2, "short YUYV buffer");
            let mut out = Vec::with_capacity(w * h * 4);
            for q in src[..w * h * 2].as_chunks::<4>().0 {
                let (y0, u, y1, v) = (q[0], q[1], q[2], q[3]);
                out.extend_from_slice(&yuv_to_bgra(y0, u, v));
                out.extend_from_slice(&yuv_to_bgra(y1, u, v));
            }
            Ok(Frame::from_bgra(width, height, out)?)
        }
        RawFormat::Nv12 => to_bgra_rect(fmt, width, height, src, (0, 0, width, height)),
    }
}

/// Decode only the rectangle `(x, y, w, h)` of a `width` x `height` YUYV or
/// NV12 buffer. The result is bit-identical to decoding the whole frame and
/// cutting the rectangle out. All four values must be even and the rectangle
/// inside the frame (as `Crop::pixels` gives); anything else, short buffers
/// and MJPEG (which cannot be decoded in parts) are errors, never panics.
pub fn to_bgra_rect(
    fmt: RawFormat,
    width: u32,
    height: u32,
    src: &[u8],
    rect: (u32, u32, u32, u32),
) -> anyhow::Result<Frame> {
    let (fw, fh) = (width as usize, height as usize);
    let (x, y, w, h) = (
        rect.0 as usize,
        rect.1 as usize,
        rect.2 as usize,
        rect.3 as usize,
    );
    anyhow::ensure!(
        w > 0
            && h > 0
            && x % 2 == 0
            && w % 2 == 0
            && x + w <= fw
            && y + h <= fh
            && (fmt != RawFormat::Nv12 || (y % 2 == 0 && h % 2 == 0 && fh % 2 == 0)),
        "bad decode rectangle {rect:?} in {width}x{height}"
    );
    let mut out = vec![0u8; w * h * 4];
    match fmt {
        RawFormat::Mjpeg => anyhow::bail!("MJPEG cannot be decoded in parts"),
        RawFormat::Yuyv => {
            anyhow::ensure!(fw % 2 == 0 && src.len() >= fw * fh * 2, "short YUYV buffer");
            for (r, orow) in out.chunks_exact_mut(w * 4).enumerate() {
                let s = ((y + r) * fw + x) * 2;
                for (q, o) in src[s..s + w * 2]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(orow.chunks_exact_mut(8))
                {
                    let (y0, u, y1, v) = (q[0], q[1], q[2], q[3]);
                    o[..4].copy_from_slice(&yuv_to_bgra(y0, u, v));
                    o[4..].copy_from_slice(&yuv_to_bgra(y1, u, v));
                }
            }
        }
        RawFormat::Nv12 => {
            anyhow::ensure!(
                fw % 2 == 0 && src.len() >= fw * fh * 3 / 2,
                "short NV12 buffer"
            );
            let (luma, chroma) = src.split_at(fw * fh);
            for (pair, opair) in out.chunks_exact_mut(w * 8).enumerate() {
                let ly = (y + pair * 2) * fw + x;
                let (l0, l1) = (&luma[ly..ly + w], &luma[ly + fw..ly + fw + w]);
                let cy = ((y / 2) + pair) * fw + x;
                let uv = &chroma[cy..cy + w];
                let (o0, o1) = opair.split_at_mut(w * 4);
                // One chroma lookup (and its three terms) per 2x2 block.
                let blocks = uv
                    .chunks_exact(2)
                    .zip(l0.chunks_exact(2).zip(l1.chunks_exact(2)))
                    .zip(o0.chunks_exact_mut(8).zip(o1.chunks_exact_mut(8)));
                for ((c, (a, b)), (p0, p1)) in blocks {
                    let d = c[0] as i32 - 128;
                    let e = c[1] as i32 - 128;
                    let rc = 409 * e + 128;
                    let gc = -100 * d - 208 * e + 128;
                    let bc = 516 * d + 128;
                    let (a0, a1) = p0.split_at_mut(4);
                    let (b0, b1) = p1.split_at_mut(4);
                    for (yy, px) in [(a[0], a0), (a[1], a1), (b[0], b0), (b[1], b1)] {
                        let l = 298 * (yy as i32 - 16);
                        px[0] = ((l + bc) >> 8).clamp(0, 255) as u8;
                        px[1] = ((l + gc) >> 8).clamp(0, 255) as u8;
                        px[2] = ((l + rc) >> 8).clamp(0, 255) as u8;
                        px[3] = 255;
                    }
                }
            }
        }
    }
    Ok(Frame::from_bgra(rect.2, rect.3, out)?)
}

/// BT.601 limited range YUV → BGRA.
fn yuv_to_bgra(y: u8, u: u8, v: u8) -> [u8; 4] {
    let c = y as i32 - 16;
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    let clamp = |x: i32| x.clamp(0, 255) as u8;
    let r = clamp((298 * c + 409 * e + 128) >> 8);
    let g = clamp((298 * c - 100 * d - 208 * e + 128) >> 8);
    let b = clamp((298 * c + 516 * d + 128) >> 8);
    [b, g, r, 255]
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED_BGRA: [u8; 4] = [0, 0, 255, 255];

    fn close(a: [u8; 4], b: [u8; 4], tol: i32) -> bool {
        a.iter()
            .zip(b)
            .all(|(x, y)| (*x as i32 - y as i32).abs() <= tol)
    }

    #[test]
    fn yuyv_red_to_bgra() {
        // BT.601 limited-range red: Y=81 U=90 V=240
        let src = [81, 90, 81, 240];
        let f = to_bgra(RawFormat::Yuyv, 2, 1, &src).unwrap();
        assert!(close(f.pixel(0, 0), RED_BGRA, 3), "{:?}", f.pixel(0, 0));
        assert!(close(f.pixel(1, 0), RED_BGRA, 3));
    }

    #[test]
    fn nv12_white_to_bgra() {
        // 2x2 white: Y=235 x4, then interleaved UV = 128,128
        let src = [235, 235, 235, 235, 128, 128];
        let f = to_bgra(RawFormat::Nv12, 2, 2, &src).unwrap();
        assert!(
            close(f.pixel(1, 1), [255, 255, 255, 255], 3),
            "{:?}",
            f.pixel(1, 1)
        );
    }

    #[test]
    fn mjpeg_red_to_bgra() {
        let mut jpg = Vec::new();
        let rgb: Vec<u8> = [255u8, 0, 0].repeat(16 * 16);
        jpeg_encoder::Encoder::new(&mut jpg, 95)
            .encode(&rgb, 16, 16, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        let f = to_bgra(RawFormat::Mjpeg, 16, 16, &jpg).unwrap();
        assert_eq!((f.width, f.height), (16, 16));
        assert!(close(f.pixel(8, 8), RED_BGRA, 12), "{:?}", f.pixel(8, 8));
    }

    #[test]
    fn short_buffer_is_error_not_panic() {
        assert!(to_bgra(RawFormat::Yuyv, 4, 4, &[0; 10]).is_err());
        assert!(to_bgra(RawFormat::Nv12, 4, 4, &[0; 10]).is_err());
        assert!(to_bgra(RawFormat::Mjpeg, 4, 4, &[1, 2, 3]).is_err());
    }

    /// Whole-frame decode, then a crop: what `to_bgra_rect` must equal.
    fn crop_after(fmt: RawFormat, w: u32, h: u32, src: &[u8], r: (u32, u32, u32, u32)) -> Vec<u8> {
        let full = if fmt == RawFormat::Nv12 {
            nv12_reference(w as usize, h as usize, src)
        } else {
            to_bgra(fmt, w, h, src).unwrap().data
        };
        crop_of(&full, w, r)
    }

    /// The pre-optimisation per-pixel NV12 decode, kept as the reference.
    fn nv12_reference(w: usize, h: usize, src: &[u8]) -> Vec<u8> {
        let (luma, chroma) = src.split_at(w * h);
        let mut out = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let c = (y / 2) * w + (x / 2) * 2;
                out.extend_from_slice(&yuv_to_bgra(luma[y * w + x], chroma[c], chroma[c + 1]));
            }
        }
        out
    }

    fn crop_of(full: &[u8], w: u32, r: (u32, u32, u32, u32)) -> Vec<u8> {
        let mut out = Vec::with_capacity(r.2 as usize * r.3 as usize * 4);
        for y in r.1..r.1 + r.3 {
            let s = (y * w + r.0) as usize * 4;
            out.extend_from_slice(&full[s..s + r.2 as usize * 4]);
        }
        out
    }

    fn noise(n: usize, seed: &mut u32) -> Vec<u8> {
        (0..n)
            .map(|_| {
                *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (*seed >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn rect_convert_equals_convert_then_crop() {
        let (w, h) = (32u32, 24u32);
        let mut seed = 7;
        let nv12 = noise((w * h * 3 / 2) as usize, &mut seed);
        let yuyv = noise((w * h * 2) as usize, &mut seed);
        let mut rects = vec![(0, 0, w, h), (0, 0, 2, 2), (w - 2, h - 2, 2, 2)];
        for _ in 0..60 {
            let rw = (noise(1, &mut seed)[0] as u32 % (w / 2) + 1) * 2;
            let rh = (noise(1, &mut seed)[0] as u32 % (h / 2) + 1) * 2;
            let x = (noise(1, &mut seed)[0] as u32 % ((w - rw) / 2 + 1)) * 2;
            let y = (noise(1, &mut seed)[0] as u32 % ((h - rh) / 2 + 1)) * 2;
            rects.push((x, y, rw, rh));
        }
        for (fmt, src) in [(RawFormat::Nv12, &nv12), (RawFormat::Yuyv, &yuyv)] {
            for r in &rects {
                let got = to_bgra_rect(fmt, w, h, src, *r).unwrap();
                assert_eq!((got.width, got.height), (r.2, r.3));
                assert_eq!(got.data, crop_after(fmt, w, h, src, *r), "{fmt:?} {r:?}");
            }
        }
    }

    #[test]
    fn rect_outside_frame_or_odd_is_error() {
        let src = vec![0u8; 8 * 8 * 3 / 2];
        for r in [(0, 0, 10, 8), (6, 0, 4, 2), (1, 0, 2, 2), (0, 0, 3, 2)] {
            assert!(
                to_bgra_rect(RawFormat::Nv12, 8, 8, &src, r).is_err(),
                "{r:?}"
            );
        }
        assert!(to_bgra_rect(RawFormat::Mjpeg, 8, 8, &src, (0, 0, 8, 8)).is_err());
    }

    #[test]
    #[ignore = "timing: run with --release"]
    fn time_nv12_1280x720() {
        let (w, h) = (1280u32, 720u32);
        let mut seed = 1;
        let src = noise((w * h * 3 / 2) as usize, &mut seed);
        for (name, r) in [
            ("full", (0, 0, w, h)),
            ("crop 960x540", (160, 90, 960, 540)),
        ] {
            let mut t = Vec::new();
            for _ in 0..60 {
                let t0 = std::time::Instant::now();
                std::hint::black_box(to_bgra_rect(RawFormat::Nv12, w, h, &src, r).unwrap());
                t.push(t0.elapsed().as_secs_f64() * 1000.0);
            }
            t.sort_by(|a, b| a.total_cmp(b));
            println!(
                "NV12 {name} (new): p50 {:.2} ms, p95 {:.2} ms",
                t[30], t[56]
            );
            // Before: per-pixel decode of the whole frame, then the crop copy.
            let mut t = Vec::new();
            for _ in 0..60 {
                let t0 = std::time::Instant::now();
                let full = nv12_reference(w as usize, h as usize, &src);
                std::hint::black_box(crop_of(&full, w, r));
                t.push(t0.elapsed().as_secs_f64() * 1000.0);
            }
            t.sort_by(|a, b| a.total_cmp(b));
            println!(
                "NV12 {name} (old): p50 {:.2} ms, p95 {:.2} ms",
                t[30], t[56]
            );
        }
    }
}
