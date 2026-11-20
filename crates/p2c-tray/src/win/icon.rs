//! Tray icons (HICON) per health level, made once per level from
//! [`crate::icon::icon_rgba`] at the small-icon size of this display.

use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};

use crate::health::Level;

pub struct Icons {
    size: u32,
    /// Per level, without and with the agent badge.
    cache: [Option<HICON>; 8],
}

fn slot(level: Level, agent: bool) -> usize {
    let l = match level {
        Level::Grey => 0,
        Level::Green => 1,
        Level::Amber => 2,
        Level::Red => 3,
    };
    l + if agent { 4 } else { 0 }
}

impl Icons {
    pub fn new() -> Icons {
        // SAFETY: plain query.
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
        Icons {
            size,
            cache: [None; 8],
        }
    }

    /// The icon for `level` (+ agent badge) (made on first use; `None` if
    /// Windows refused).
    pub fn get(&mut self, level: Level, agent: bool) -> Option<HICON> {
        let i = slot(level, agent);
        if self.cache[i].is_none() {
            match hicon(
                &crate::icon::icon_rgba_for(level, self.size, agent),
                self.size,
            ) {
                Ok(h) => self.cache[i] = Some(h),
                Err(e) => tracing::warn!("cannot make the tray icon: {e}"),
            }
        }
        self.cache[i]
    }
}

impl Drop for Icons {
    fn drop(&mut self) {
        for h in self.cache.iter().flatten() {
            // SAFETY: icons we created.
            unsafe {
                let _ = DestroyIcon(*h);
            }
        }
    }
}

/// An HICON from straight-alpha RGBA pixels (`size` x `size`).
fn hicon(rgba: &[u8], size: u32) -> windows::core::Result<HICON> {
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: size as i32,
        biHeight: -(size as i32), // top-down
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        ..Default::default()
    };
    let info = BITMAPINFO {
        bmiHeader: header,
        ..Default::default()
    };
    // SAFETY: `bits` points to size*size*4 bytes owned by the DIB section
    // until it is deleted below; the mask is a plain 1-bpp bitmap.
    unsafe {
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, rgba.len());
        for (d, s) in dst
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(rgba.as_chunks::<4>().0)
        {
            *d = [s[2], s[1], s[0], s[3]]; // BGRA
        }
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, None);
        let ii = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&ii);
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        icon
    }
}
