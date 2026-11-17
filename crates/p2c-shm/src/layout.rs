use std::sync::atomic::{fence, AtomicU64, Ordering};

pub const MAX_WIDTH: u32 = 1920;
pub const MAX_HEIGHT: u32 = 1080;
pub const HEADER_SIZE: usize = 64;
pub const MAPPING_SIZE: usize = HEADER_SIZE + (MAX_WIDTH * MAX_HEIGHT * 4) as usize;

const MAGIC: u32 = u32::from_le_bytes(*b"P2CF");
const VERSION: u32 = 1;

/// Start of the mapping. `seq` is a seqlock: odd while a frame is being written.
#[repr(C)]
pub(crate) struct Header {
    magic: u32,
    version: u32,
    width: u32,
    height: u32,
    pub(crate) seq: AtomicU64,
    time_ms: AtomicU64,
    _pad: [u8; 32],
}

const _: () = assert!(std::mem::size_of::<Header>() == HEADER_SIZE);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    /// Increases by 2 per frame; never odd here.
    pub seq: u64,
    /// Writer's clock (ms since the Unix epoch) when the frame was written.
    pub time_ms: u64,
}

/// Write one tightly packed BGRA frame. Returns false if it doesn't fit.
///
/// # Safety
/// `base` must point to a writable, 8-byte aligned mapping of `MAPPING_SIZE` bytes.
pub unsafe fn write_frame(
    base: *mut u8,
    width: u32,
    height: u32,
    bgra: &[u8],
    time_ms: u64,
) -> bool {
    let len = width as usize * height as usize * 4;
    if width > MAX_WIDTH || height > MAX_HEIGHT || bgra.len() < len {
        return false;
    }
    let h = &mut *(base as *mut Header);
    let s = h.seq.load(Ordering::Relaxed);
    let s = if s % 2 == 1 { s + 1 } else { s };
    h.seq.store(s + 1, Ordering::Relaxed);
    fence(Ordering::Release);
    h.magic = MAGIC;
    h.version = VERSION;
    h.width = width;
    h.height = height;
    std::ptr::copy_nonoverlapping(bgra.as_ptr(), base.add(HEADER_SIZE), len);
    h.time_ms.store(time_ms, Ordering::Relaxed);
    fence(Ordering::Release);
    h.seq.store(s + 2, Ordering::Release);
    true
}

/// Copy the newest complete frame into `out`. `None` if there is no frame yet
/// or a write was in progress (the caller keeps its previous frame).
///
/// # Safety
/// `base` must point to a readable, 8-byte aligned mapping of `MAPPING_SIZE` bytes.
pub unsafe fn read_frame(base: *const u8, out: &mut Vec<u8>) -> Option<FrameInfo> {
    let h = &*(base as *const Header);
    let s1 = h.seq.load(Ordering::Acquire);
    if s1 == 0 || s1 % 2 == 1 {
        return None;
    }
    fence(Ordering::Acquire);
    let (magic, version) = (
        std::ptr::read_volatile(&h.magic),
        std::ptr::read_volatile(&h.version),
    );
    let (width, height) = (
        std::ptr::read_volatile(&h.width),
        std::ptr::read_volatile(&h.height),
    );
    if magic != MAGIC
        || version != VERSION
        || width == 0
        || height == 0
        || width > MAX_WIDTH
        || height > MAX_HEIGHT
    {
        return None;
    }
    let len = width as usize * height as usize * 4;
    out.resize(len, 0);
    std::ptr::copy_nonoverlapping(base.add(HEADER_SIZE), out.as_mut_ptr(), len);
    let time_ms = h.time_ms.load(Ordering::Relaxed);
    fence(Ordering::Acquire);
    if h.seq.load(Ordering::Acquire) != s1 {
        return None;
    }
    Some(FrameInfo {
        width,
        height,
        seq: s1,
        time_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Vec<u64> {
        vec![0u64; MAPPING_SIZE / 8 + 1] // u64 for alignment
    }

    #[test]
    fn roundtrip() {
        let mut m = mem();
        let base = m.as_mut_ptr() as *mut u8;
        let px: Vec<u8> = (0..4 * 4 * 2).map(|i| i as u8).collect();
        unsafe { write_frame(base, 4, 2, &px, 1234) };
        let mut out = Vec::new();
        let info = unsafe { read_frame(base, &mut out) }.unwrap();
        assert_eq!(
            info,
            FrameInfo {
                width: 4,
                height: 2,
                seq: 2,
                time_ms: 1234
            }
        );
        assert_eq!(out, px);
    }

    #[test]
    fn empty_mapping_has_no_frame() {
        let mut m = mem();
        assert!(unsafe { read_frame(m.as_mut_ptr() as *const u8, &mut Vec::new()) }.is_none());
    }

    #[test]
    fn torn_write_is_rejected() {
        let mut m = mem();
        let base = m.as_mut_ptr() as *mut u8;
        unsafe { write_frame(base, 2, 2, &[7; 16], 1) };
        // Simulate a writer in progress: odd sequence number.
        unsafe {
            (*(base as *const Header))
                .seq
                .store(3, std::sync::atomic::Ordering::SeqCst)
        };
        assert!(unsafe { read_frame(base, &mut Vec::new()) }.is_none());
    }

    #[test]
    fn oversized_frame_is_refused() {
        let mut m = mem();
        let base = m.as_mut_ptr() as *mut u8;
        let big = vec![0u8; ((MAX_WIDTH + 2) * 2 * 4) as usize];
        assert!(!unsafe { write_frame(base, MAX_WIDTH + 2, 2, &big, 1) });
        assert!(unsafe { read_frame(base, &mut Vec::new()) }.is_none());
    }
}
