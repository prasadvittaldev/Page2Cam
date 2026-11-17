//! Live preview: a ticker sink that samples frames, and a JPEG encoder thread
//! that only works while an admin page is watching.
use std::time::Duration;

use p2c_compositor::{scale_frame, FrameSink};
use p2c_core::{Frame, FrameSlot};
use tokio::sync::watch;

/// Puts every `every`-th composited frame into a slot (cheap; no encoding here).
pub struct PreviewSink {
    slot: FrameSlot,
    every: u32,
    n: u32,
}

impl PreviewSink {
    pub fn new(slot: FrameSlot, every: u32) -> Self {
        PreviewSink {
            slot,
            every: every.max(1),
            n: 0,
        }
    }
}

impl FrameSink for PreviewSink {
    fn push(&mut self, frame: &Frame) -> anyhow::Result<()> {
        self.n = self.n.wrapping_add(1);
        if self.n.is_multiple_of(self.every) {
            self.slot.put(frame.clone());
        }
        Ok(())
    }
}

pub(crate) const PREVIEW_WIDTH: u32 = 960;

/// Encode `f` scaled to `PREVIEW_WIDTH` wide as JPEG.
pub(crate) fn encode_preview(f: &Frame) -> anyhow::Result<Vec<u8>> {
    let w = PREVIEW_WIDTH.min(f.width);
    let h = ((f.height as u64 * w as u64) / f.width.max(1) as u64).max(1) as u32;
    let small = if w == f.width {
        f.clone()
    } else {
        scale_frame(f, w, h)
    };
    let mut out = Vec::with_capacity(64 * 1024);
    jpeg_encoder::Encoder::new(&mut out, 70).encode(
        &small.data,
        small.width as u16,
        small.height as u16,
        jpeg_encoder::ColorType::Bgra,
    )?;
    Ok(out)
}

/// Encoder thread: while anyone is subscribed, publish the newest frame as JPEG.
pub(crate) fn spawn_encoder(
    slot: FrameSlot,
    tx: std::sync::Arc<watch::Sender<Option<std::sync::Arc<Vec<u8>>>>>,
) {
    std::thread::Builder::new()
        .name("p2c-preview".into())
        .spawn(move || {
            let mut last_seq = 0;
            loop {
                std::thread::sleep(Duration::from_millis(80));
                if tx.receiver_count() == 0 {
                    continue;
                }
                let seq = slot.seq();
                if seq == last_seq {
                    continue;
                }
                last_seq = seq;
                if let Some(f) = slot.latest() {
                    match encode_preview(&f) {
                        Ok(jpg) => {
                            let _ = tx.send(Some(std::sync::Arc::new(jpg)));
                        }
                        Err(e) => tracing::debug!(error = %e, "preview encode failed"),
                    }
                }
            }
        })
        .expect("spawn preview thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sink_samples_every_nth() {
        let slot = FrameSlot::new();
        let mut s = PreviewSink::new(slot.clone(), 3);
        for _ in 0..9 {
            s.push(&Frame::new_black(4, 4)).unwrap();
        }
        assert_eq!(slot.seq(), 3);
    }

    #[test]
    fn preview_is_jpeg_960_wide() {
        let jpg = encode_preview(&Frame::new_black(1920, 1080)).unwrap();
        assert_eq!(&jpg[..2], &[0xFF, 0xD8]);
        // SOF0 marker carries height then width
        let sof = jpg.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        let h = u16::from_be_bytes([jpg[sof + 5], jpg[sof + 6]]);
        let w = u16::from_be_bytes([jpg[sof + 7], jpg[sof + 8]]);
        assert_eq!((w, h), (960, 540));
    }
}
