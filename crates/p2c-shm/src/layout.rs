use std::sync::atomic::{fence, AtomicU64, Ordering};

pub const MAX_WIDTH: u32 = 1920;
pub const MAX_HEIGHT: u32 = 1080;
pub const HEADER_SIZE: usize = 64;
pub const MAPPING_SIZE: usize = HEADER_SIZE + (MAX_WIDTH * MAX_HEIGHT * 4) as usize;

const MAGIC: u32 = u32::from_le_bytes(*b"P2CF");
/// Protocol version. v2 adds the pixel `format` and the DLL's `dll_version`.
pub const VERSION: u32 = 2;

/// Pixel layout of the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum PixelFormat {
    /// Packed BGRA8, `w*h*4` bytes.
    Bgra = 1,
    /// NV12 (BT.709 limited range), tightly packed: `w*h` Y then `w*h/2` UV.
    Nv12 = 2,
}

impl PixelFormat {
    fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::Bgra),
            2 => Some(Self::Nv12),
            _ => None,
        }
    }

    fn frame_len(self, width: u32, height: u32) -> usize {
        let px = width as usize * height as usize;
        match self {
            Self::Bgra => px * 4,
            Self::Nv12 => px * 3 / 2,
        }
    }
}

/// Start of the mapping. `seq` is a seqlock: odd while a frame is being written.
#[repr(C)]
pub(crate) struct Header {
    magic: u32,
    version: u32,
    width: u32,
    height: u32,
    pub(crate) seq: AtomicU64,
    time_ms: AtomicU64,
    format: u32,
    /// Written once by the DLL when it creates the mapping; 0 = old DLL.
    dll_version: u32,
    _pad: [u8; 24],
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
    pub format: PixelFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// No (valid) frame has been written yet.
    NoFrame,
    /// A write was in progress or tore the read; keep the previous frame.
    Busy,
    /// The writer uses another protocol version.
    Version { found: u32 },
}

/// Write one tightly packed frame. Returns false if it doesn't fit or the
/// size is invalid (NV12 needs even width/height).
///
/// # Safety
/// `base` must point to a writable, 8-byte aligned mapping of `MAPPING_SIZE` bytes.
pub unsafe fn write_frame(
    base: *mut u8,
    fmt: PixelFormat,
    width: u32,
    height: u32,
    data: &[u8],
    time_ms: u64,
) -> bool {
    let len = fmt.frame_len(width, height);
    if width == 0
        || height == 0
        || width > MAX_WIDTH
        || height > MAX_HEIGHT
        || data.len() < len
        || (fmt == PixelFormat::Nv12 && (!width.is_multiple_of(2) || !height.is_multiple_of(2)))
    {
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
    h.format = fmt as u32;
    std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(HEADER_SIZE), len);
    h.time_ms.store(time_ms, Ordering::Relaxed);
    fence(Ordering::Release);
    h.seq.store(s + 2, Ordering::Release);
    true
}

/// Copy the newest complete frame into `out`.
///
/// # Safety
/// `base` must point to a readable, 8-byte aligned mapping of `MAPPING_SIZE` bytes.
pub unsafe fn read_frame(base: *const u8, out: &mut Vec<u8>) -> Result<FrameInfo, ReadError> {
    let h = &*(base as *const Header);
    let s1 = h.seq.load(Ordering::Acquire);
    if s1 == 0 {
        return Err(ReadError::NoFrame);
    }
    if s1 % 2 == 1 {
        return Err(ReadError::Busy);
    }
    fence(Ordering::Acquire);
    let magic = std::ptr::read_volatile(&h.magic);
    let version = std::ptr::read_volatile(&h.version);
    if magic != MAGIC {
        return Err(ReadError::NoFrame);
    }
    if version != VERSION {
        return Err(ReadError::Version { found: version });
    }
    let (width, height) = (
        std::ptr::read_volatile(&h.width),
        std::ptr::read_volatile(&h.height),
    );
    let format = std::ptr::read_volatile(&h.format);
    let Some(fmt) = PixelFormat::from_u32(format) else {
        return Err(ReadError::Busy);
    };
    if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
        return Err(ReadError::Busy);
    }
    let len = fmt.frame_len(width, height);
    out.resize(len, 0);
    std::ptr::copy_nonoverlapping(base.add(HEADER_SIZE), out.as_mut_ptr(), len);
    let time_ms = h.time_ms.load(Ordering::Relaxed);
    fence(Ordering::Acquire);
    if h.seq.load(Ordering::Acquire) != s1 {
        return Err(ReadError::Busy);
    }
    Ok(FrameInfo {
        width,
        height,
        seq: s1,
        time_ms,
        format: fmt,
    })
}

/// DLL side: stamp the protocol version into a freshly created mapping.
///
/// # Safety
/// `base` must point to a writable, 8-byte aligned mapping of `MAPPING_SIZE` bytes.
pub unsafe fn set_dll_version(base: *mut u8) {
    let h = &mut *(base as *mut Header);
    std::ptr::write_volatile(&mut h.dll_version, VERSION);
}

/// Protocol version the DLL stamped into the mapping (0 = old DLL).
///
/// # Safety
/// `base` must point to a readable, 8-byte aligned mapping of `MAPPING_SIZE` bytes.
pub unsafe fn dll_version(base: *const u8) -> u32 {
    let h = &*(base as *const Header);
    std::ptr::read_volatile(&h.dll_version)
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
        unsafe { write_frame(base, PixelFormat::Bgra, 4, 2, &px, 1234) };
        let mut out = Vec::new();
        let info = unsafe { read_frame(base, &mut out) }.unwrap();
        assert_eq!(
            info,
            FrameInfo {
                width: 4,
                height: 2,
                seq: 2,
                time_ms: 1234,
                format: PixelFormat::Bgra,
            }
        );
        assert_eq!(out, px);
    }

    #[test]
    fn nv12_roundtrip() {
        let mut m = mem();
        let base = m.as_mut_ptr() as *mut u8;
        let px: Vec<u8> = (0..12).map(|i| i as u8 + 1).collect();
        assert!(unsafe { write_frame(base, PixelFormat::Nv12, 4, 2, &px, 99) });
        let mut out = Vec::new();
        let info = unsafe { read_frame(base, &mut out) }.unwrap();
        assert_eq!(info.format, PixelFormat::Nv12);
        assert_eq!(info.seq, 2);
        assert_eq!(out, px);
    }

    #[test]
    fn nv12_odd_size_is_refused() {
        let mut m = mem();
        let base = m.as_mut_ptr() as *mut u8;
        assert!(!unsafe { write_frame(base, PixelFormat::Nv12, 3, 2, &[0; 64], 1) });
        assert!(!unsafe { write_frame(base, PixelFormat::Nv12, 4, 2, &[0; 11], 1) });
    }

    #[test]
    fn empty_mapping_has_no_frame() {
        let mut m = mem();
        assert_eq!(
            unsafe { read_frame(m.as_mut_ptr() as *const u8, &mut Vec::new()) },
            Err(ReadError::NoFrame)
        );
    }

    #[test]
    fn read_rejects_v1_and_reports_version() {
        let mut m = mem();
        let base = m.as_mut_ptr() as *mut u8;
        unsafe {
            let h = &mut *(base as *mut Header);
            h.magic = MAGIC;
            h.version = 1;
            h.width = 2;
            h.height = 2;
            h.seq.store(2, Ordering::SeqCst);
        }
        assert_eq!(
            unsafe { read_frame(base, &mut Vec::new()) },
            Err(ReadError::Version { found: 1 })
        );
    }

    #[test]
    fn dll_version_defaults_to_zero_and_can_be_set() {
        let mut m = mem();
        let base = m.as_mut_ptr() as *mut u8;
        assert_eq!(unsafe { dll_version(base) }, 0);
        unsafe { set_dll_version(base) };
        assert_eq!(unsafe { dll_version(base) }, VERSION);
        // Writing frames must not clobber it.
        unsafe { write_frame(base, PixelFormat::Bgra, 2, 2, &[7; 16], 1) };
        assert_eq!(unsafe { dll_version(base) }, VERSION);
    }

    #[test]
    fn torn_write_is_rejected() {
        let mut m = mem();
        let base = m.as_mut_ptr() as *mut u8;
        unsafe { write_frame(base, PixelFormat::Bgra, 2, 2, &[7; 16], 1) };
        // Simulate a writer in progress: odd sequence number.
        unsafe {
            (*(base as *const Header))
                .seq
                .store(3, std::sync::atomic::Ordering::SeqCst)
        };
        assert_eq!(
            unsafe { read_frame(base, &mut Vec::new()) },
            Err(ReadError::Busy)
        );
    }

    #[test]
    fn oversized_frame_is_refused() {
        let mut m = mem();
        let base = m.as_mut_ptr() as *mut u8;
        let big = vec![0u8; ((MAX_WIDTH + 2) * 2 * 4) as usize];
        assert!(!unsafe { write_frame(base, PixelFormat::Bgra, MAX_WIDTH + 2, 2, &big, 1) });
        assert_eq!(
            unsafe { read_frame(base, &mut Vec::new()) },
            Err(ReadError::NoFrame)
        );
    }
}
