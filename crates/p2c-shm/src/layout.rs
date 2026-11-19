use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};

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
    /// Who may write frames (`Writer`); set only by the tray. Old DLLs ignore it.
    writer: AtomicU32,
    _pad: [u8; 4],
    /// Wall clock (ms) when the current writer took the lock (odd `seq`); 0
    /// when not writing. Lets another writer reclaim a lock a dead one left.
    lock_since_ms: AtomicU64,
    _pad2: [u8; 8],
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
    /// The writer uses a pixel format this reader doesn't know (a newer app):
    /// like `Version`, app and camera driver must be installed together.
    Format { found: u32 },
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
    write_frame_at(base, fmt, width, height, data, time_ms, wall_ms())
}

/// `write_frame` with an injected clock (for the stale-lock test).
unsafe fn write_frame_at(
    base: *mut u8,
    fmt: PixelFormat,
    width: u32,
    height: u32,
    data: &[u8],
    time_ms: u64,
    now_ms: u64,
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
    let hp = base as *mut Header;
    let h = &*hp;
    // Try-lock: seq even -> odd. Another writer mid-write (odd) or a lost race
    // means skip this frame, so two writers never overlap.
    let s = h.seq.load(Ordering::Acquire);
    let s = if s.is_multiple_of(2) {
        if h.seq
            .compare_exchange(s, s + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        s
    } else {
        // Odd: a writer is busy, or died mid-write (e.g. killed). Reclaim only
        // after the lock has been held for more than STALE_LOCK_MS.
        let since = h.lock_since_ms.load(Ordering::Acquire);
        if since == 0
            || now_ms.saturating_sub(since) <= STALE_LOCK_MS
            || h.seq
                .compare_exchange(s, s + 2, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return false;
        }
        s + 1
    };
    h.lock_since_ms.store(now_ms, Ordering::Release);
    // Fields are only touched while holding the lock; readers re-check seq.
    std::ptr::write_volatile(std::ptr::addr_of_mut!((*hp).magic), MAGIC);
    std::ptr::write_volatile(std::ptr::addr_of_mut!((*hp).version), VERSION);
    std::ptr::write_volatile(std::ptr::addr_of_mut!((*hp).width), width);
    std::ptr::write_volatile(std::ptr::addr_of_mut!((*hp).height), height);
    std::ptr::write_volatile(std::ptr::addr_of_mut!((*hp).format), fmt as u32);
    std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(HEADER_SIZE), len);
    h.time_ms.store(time_ms, Ordering::Relaxed);
    h.lock_since_ms.store(0, Ordering::Relaxed);
    fence(Ordering::Release);
    h.seq.store(s + 2, Ordering::Release);
    true
}

/// A held write lock (odd `seq`) older than this is taken to belong to a dead
/// writer and may be reclaimed. `Header::lock_since_ms` (wall clock, ms) is set
/// right after taking the lock and cleared at the end; 0 while odd (the tiny
/// window right after the CAS) is never reclaimed.
const STALE_LOCK_MS: u64 = 500;

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
        // Only a consistent read counts (a torn one is just Busy).
        fence(Ordering::Acquire);
        if h.seq.load(Ordering::Acquire) != s1 {
            return Err(ReadError::Busy);
        }
        return Err(ReadError::Format { found: format });
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
    let hp = base as *mut Header;
    std::ptr::write_volatile(std::ptr::addr_of_mut!((*hp).dll_version), VERSION);
}

/// Protocol version the DLL stamped into the mapping (0 = old DLL).
///
/// # Safety
/// `base` must point to a readable, 8-byte aligned mapping of `MAPPING_SIZE` bytes.
pub unsafe fn dll_version(base: *const u8) -> u32 {
    let h = &*(base as *const Header);
    std::ptr::read_volatile(&h.dll_version)
}

/// Who currently owns frame writing. The tray hands it to the engine once
/// the engine produces output, and takes it back when the engine stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Writer {
    None = 0,
    Tray = 1,
    Engine = 2,
}

/// Tray side: say who writes frames now.
///
/// # Safety
/// `base` must point to a writable, 8-byte aligned mapping of `MAPPING_SIZE` bytes.
pub unsafe fn set_writer(base: *mut u8, w: Writer) {
    (*(base as *const Header))
        .writer
        .store(w as u32, Ordering::Release);
}

/// Who may write frames (unknown values read as `Writer::None`).
///
/// # Safety
/// `base` must point to a readable, 8-byte aligned mapping of `MAPPING_SIZE` bytes.
pub unsafe fn writer(base: *const u8) -> Writer {
    let h = &*(base as *const Header);
    match h.writer.load(Ordering::Acquire) {
        1 => Writer::Tray,
        2 => Writer::Engine,
        _ => Writer::None,
    }
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
    fn unknown_format_is_reported_not_busy() {
        // A newer app with a pixel format this DLL doesn't know: not a stall.
        let mut m = mem();
        let base = m.as_mut_ptr() as *mut u8;
        unsafe {
            write_frame(base, PixelFormat::Bgra, 2, 2, &[7; 16], 1);
            (*(base as *mut Header)).format = 7;
        }
        assert_eq!(
            unsafe { read_frame(base, &mut Vec::new()) },
            Err(ReadError::Format { found: 7 })
        );
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
    fn writer_field_roundtrip_and_header_size() {
        assert_eq!(std::mem::size_of::<Header>(), HEADER_SIZE);
        let mut m = mem();
        let base = m.as_mut_ptr() as *mut u8;
        assert_eq!(unsafe { writer(base) }, Writer::None);
        for w in [Writer::Tray, Writer::Engine, Writer::None] {
            unsafe { set_writer(base, w) };
            assert_eq!(unsafe { writer(base) }, w);
        }
        assert_eq!(Writer::Tray as u32, 1);
        assert_eq!(Writer::Engine as u32, 2);
        // Frames and the DLL stamp don't clobber it.
        unsafe { set_writer(base, Writer::Engine) };
        unsafe { set_dll_version(base) };
        unsafe { write_frame(base, PixelFormat::Bgra, 2, 2, &[7; 16], 1) };
        assert_eq!(unsafe { writer(base) }, Writer::Engine);
        // An unknown value reads as None.
        unsafe { (*(base as *const Header)).writer.store(9, Ordering::SeqCst) };
        assert_eq!(unsafe { writer(base) }, Writer::None);
    }

    #[test]
    fn old_dll_unaffected_by_writer_field() {
        let mut m = mem();
        let base = m.as_mut_ptr() as *mut u8;
        let px: Vec<u8> = (0..16).collect();
        unsafe {
            set_writer(base, Writer::Tray);
            write_frame(base, PixelFormat::Bgra, 2, 2, &px, 5);
        }
        // The v2 reader (the DLL's) sees the same frame and version.
        let mut out = Vec::new();
        let info = unsafe { read_frame(base, &mut out) }.unwrap();
        assert_eq!((info.width, info.height, info.time_ms), (2, 2, 5));
        assert_eq!(out, px);
        assert_eq!(VERSION, 2);
        unsafe { set_writer(base, Writer::Engine) };
        assert!(unsafe { read_frame(base, &mut out) }.is_ok());
    }

    #[test]
    fn concurrent_writers_never_overlap() {
        let mut m = mem();
        let base = m.as_mut_ptr() as usize;
        let stop = std::sync::atomic::AtomicBool::new(false);
        let wrote = [AtomicU64::new(0), AtomicU64::new(0)];
        std::thread::scope(|sc| {
            for (i, fill) in [0xAAu8, 0x55u8].into_iter().enumerate() {
                let (stop, wrote) = (&stop, &wrote);
                sc.spawn(move || {
                    let px = vec![fill; 64 * 64 * 4];
                    while !stop.load(Ordering::Relaxed) {
                        if unsafe {
                            write_frame(base as *mut u8, PixelFormat::Bgra, 64, 64, &px, 1)
                        } {
                            wrote[i].fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
            let mut out = Vec::new();
            let mut good = 0;
            let t0 = std::time::Instant::now();
            while t0.elapsed() < std::time::Duration::from_millis(500) {
                if unsafe { read_frame(base as *const u8, &mut out) }.is_ok() {
                    assert!(
                        out.iter().all(|&b| b == out[0]) && (out[0] == 0xAA || out[0] == 0x55),
                        "mixed frame"
                    );
                    good += 1;
                }
            }
            stop.store(true, Ordering::Relaxed);
            assert!(good > 0);
        });
        assert!(wrote[0].load(Ordering::Relaxed) + wrote[1].load(Ordering::Relaxed) > 0);
        let h = unsafe { &*(base as *const Header) };
        assert_eq!(h.seq.load(Ordering::Relaxed) % 2, 0, "seq left odd");
    }

    #[test]
    fn stale_odd_seq_is_reclaimed_after_500ms() {
        let mut m = mem();
        let base = m.as_mut_ptr() as *mut u8;
        let px = [7u8; 16];
        let h = unsafe { &*(base as *const Header) };
        // A writer that began at t=1000 and died: odd seq.
        h.seq.store(3, Ordering::SeqCst);
        h.lock_since_ms.store(1000, Ordering::SeqCst);
        // Within 500 ms the other writer is assumed alive: skip.
        assert!(!unsafe { write_frame_at(base, PixelFormat::Bgra, 2, 2, &px, 1, 1400) });
        assert_eq!(h.seq.load(Ordering::SeqCst), 3);
        // Past 500 ms it is reclaimed and the frame is written.
        assert!(unsafe { write_frame_at(base, PixelFormat::Bgra, 2, 2, &px, 1, 1501) });
        assert_eq!(h.seq.load(Ordering::SeqCst) % 2, 0);
        let mut out = Vec::new();
        assert!(unsafe { read_frame(base, &mut out) }.is_ok());
        assert_eq!(out, px);
        // Normal writes work again.
        assert!(unsafe { write_frame(base, PixelFormat::Bgra, 2, 2, &px, 2) });
    }

    #[test]
    fn writer_is_atomic_and_stays_in_64_bytes() {
        assert_eq!(std::mem::size_of::<Header>(), 64);
        let w = AtomicU32::new(0);
        w.store(2, Ordering::Release);
        assert_eq!(w.load(Ordering::Acquire), 2);
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
