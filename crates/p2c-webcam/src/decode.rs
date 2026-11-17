//! Raw webcam buffers → BGRA, in pure Rust (no C JPEG library to cross-compile).
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
        RawFormat::Nv12 => {
            anyhow::ensure!(
                w % 2 == 0 && h % 2 == 0 && src.len() >= w * h * 3 / 2,
                "short NV12 buffer"
            );
            let (luma, chroma) = src.split_at(w * h);
            let mut out = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                for x in 0..w {
                    let c = (y / 2) * w + (x / 2) * 2;
                    out.extend_from_slice(&yuv_to_bgra(luma[y * w + x], chroma[c], chroma[c + 1]));
                }
            }
            Ok(Frame::from_bgra(width, height, out)?)
        }
    }
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
}
