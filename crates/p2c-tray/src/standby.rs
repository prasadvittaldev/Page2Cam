//! Standby pictures shown on the virtual camera while the video engine is not
//! running. Platform-free: the PNGs are embedded and converted to NV12 once.

use p2c_core::{bgra_to_nv12, Frame, Nv12Frame};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandbyKind {
    Stopped,
    Attention,
}

const STOPPED_PNG: &[u8] = include_bytes!("../assets/standby-stopped.png");
const ATTENTION_PNG: &[u8] = include_bytes!("../assets/standby-attention.png");

/// The standby picture as NV12 (1920x1080). Decoded and converted on first use.
pub fn standby_nv12(kind: StandbyKind) -> &'static Nv12Frame {
    static STOPPED: OnceLock<Nv12Frame> = OnceLock::new();
    static ATTENTION: OnceLock<Nv12Frame> = OnceLock::new();
    let (cell, png) = match kind {
        StandbyKind::Stopped => (&STOPPED, STOPPED_PNG),
        StandbyKind::Attention => (&ATTENTION, ATTENTION_PNG),
    };
    cell.get_or_init(|| decode(png))
}

fn decode(png: &[u8]) -> Nv12Frame {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .expect("embedded standby PNG decodes")
        .to_rgba8();
    let (w, h) = img.dimensions();
    let mut data = img.into_raw();
    for px in data.as_chunks_mut::<4>().0 {
        px.swap(0, 2); // RGBA -> BGRA
    }
    let frame = Frame::from_bgra(w, h, data).expect("RGBA buffer is packed");
    bgra_to_nv12(&frame, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standby_frames_decode_to_1080p_nv12() {
        let s = standby_nv12(StandbyKind::Stopped);
        let a = standby_nv12(StandbyKind::Attention);
        for f in [s, a] {
            assert_eq!((f.width, f.height), (1920, 1080));
            assert_eq!(f.data.len(), 1920 * 1080 * 3 / 2);
            let y = &f.data[..1920 * 1080];
            assert!(y.iter().all(|v| (16..=235).contains(v)));
            let mean = y.iter().map(|&v| v as u64).sum::<u64>() / y.len() as u64;
            assert!((40..100).contains(&mean), "mean luma {mean} should be dark");
            // Navy corner: U > 128, V < 128 (catches an RGBA/BGRA swap).
            let (u, v) = (f.data[1920 * 1080], f.data[1920 * 1080 + 1]);
            assert!(u > 128 && v < 128, "navy chroma u={u} v={v}");
            assert!(
                *y.iter().max().unwrap() > 200,
                "expected bright text pixels"
            );
        }
        assert_ne!(s.data, a.data);
    }
}
