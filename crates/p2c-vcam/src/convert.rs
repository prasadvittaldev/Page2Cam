use p2c_core::Frame;

/// Convert BGRA to packed YUYV (YUY2), BT.601 limited range.
/// Chroma is averaged across each horizontal pixel pair. Width must be even.
pub fn bgra_to_yuyv(src: &Frame, dst: &mut Vec<u8>) {
    debug_assert!(src.width % 2 == 0, "YUYV requires even width");
    dst.resize(src.width as usize * src.height as usize * 2, 0);
    let (pairs, _) = src.data.as_chunks::<8>();
    let (out, _) = dst.as_chunks_mut::<4>();
    for (p, o) in pairs.iter().zip(out.iter_mut()) {
        let (b0, g0, r0) = (p[0] as i32, p[1] as i32, p[2] as i32);
        let (b1, g1, r1) = (p[4] as i32, p[5] as i32, p[6] as i32);
        let (r, g, b) = ((r0 + r1) / 2, (g0 + g1) / 2, (b0 + b1) / 2);
        o[0] = luma(r0, g0, b0);
        o[1] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
        o[2] = luma(r1, g1, b1);
        o[3] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;
    }
}

#[inline]
fn luma(r: i32, g: i32, b: i32) -> u8 {
    (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2c_core::Frame;

    fn solid(b: u8, g: u8, r: u8) -> Frame {
        Frame::from_bgra(2, 1, [b, g, r, 255, b, g, r, 255].to_vec()).unwrap()
    }

    fn assert_close(got: &[u8], want: [u8; 4]) {
        for (g, w) in got.iter().zip(want) {
            assert!(
                (*g as i32 - w as i32).abs() <= 1,
                "got {got:?}, want {want:?}"
            );
        }
    }

    fn convert(f: &Frame) -> Vec<u8> {
        let mut dst = Vec::new();
        bgra_to_yuyv(f, &mut dst);
        dst
    }

    #[test]
    fn white() {
        assert_close(&convert(&solid(255, 255, 255)), [235, 128, 235, 128]);
    }

    #[test]
    fn black() {
        assert_close(&convert(&solid(0, 0, 0)), [16, 128, 16, 128]);
    }

    #[test]
    fn red() {
        assert_close(&convert(&solid(0, 0, 255)), [81, 90, 81, 240]);
    }

    #[test]
    fn output_len() {
        assert_eq!(convert(&Frame::new_black(4, 2)).len(), 16);
    }
}
