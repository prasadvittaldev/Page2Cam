//! Windows: the capturable top-level windows and a window's state.
use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use windows::core::{BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowLongW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};

use crate::{WindowInfo, WindowStatus};

/// Shell windows that are never offered.
const SHELL_CLASSES: [&str; 2] = ["Progman", "Shell_TrayWnd"];

pub(crate) fn hwnd(id: u64) -> HWND {
    HWND(id as usize as *mut core::ffi::c_void)
}

fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Full path of a process's executable, if we may ask.
fn exe_path(pid: u32) -> Option<String> {
    // SAFETY: a query-only handle, closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `len` is the buffer size in characters.
    let ok = unsafe {
        QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
    };
    // SAFETY: our handle.
    let _ = unsafe { CloseHandle(h) };
    ok.ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// The FileDescription from an executable's version resource.
fn file_description(path: &str) -> Option<String> {
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    // SAFETY: NUL-terminated path; the buffer has the size asked for.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(wide.as_ptr()), None, size, data.as_mut_ptr().cast()).ok()?;
        let query = |sub: &str| -> Option<(*const u8, u32)> {
            let sub: Vec<u16> = sub.encode_utf16().chain([0]).collect();
            let mut p = std::ptr::null_mut();
            let mut n = 0u32;
            VerQueryValueW(data.as_ptr().cast(), PCWSTR(sub.as_ptr()), &mut p, &mut n)
                .as_bool()
                .then_some((p as *const u8, n))
        };
        // The first language/code page, then common fallbacks.
        let mut langs = Vec::new();
        if let Some((p, n)) = query(r"\VarFileInfo\Translation") {
            if n >= 4 {
                let lang = u16::from_le_bytes([*p, *p.add(1)]);
                let cp = u16::from_le_bytes([*p.add(2), *p.add(3)]);
                langs.push(format!("{lang:04x}{cp:04x}"));
            }
        }
        langs.extend(["040904b0".to_string(), "040904e4".to_string()]);
        for l in langs {
            if let Some((p, n)) = query(&format!(r"\StringFileInfo\{l}\FileDescription")) {
                if n > 0 {
                    let s = std::slice::from_raw_parts(p as *const u16, n as usize);
                    let d = wide_to_string(s).trim().to_string();
                    if !d.is_empty() {
                        return Some(d);
                    }
                }
            }
        }
        None
    }
}

fn cloaked(h: HWND) -> bool {
    let mut v = 0u32;
    // SAFETY: DWMWA_CLOAKED fills a u32.
    unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            (&mut v as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    }
    .is_ok()
        && v != 0
}

/// The window's visible bounds (without the invisible resize borders).
fn size(h: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    // SAFETY: the attribute fills a RECT.
    let dwm = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut r as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if dwm.is_err() {
        // SAFETY: plain query.
        let _ = unsafe { GetWindowRect(h, &mut r) };
    }
    (
        (r.right - r.left).max(0) as u32,
        (r.bottom - r.top).max(0) as u32,
    )
}

pub fn list() -> Result<Vec<WindowInfo>> {
    unsafe extern "system" fn add(h: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the Vec passed below, alive for the whole call.
        let out = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
        out.push(h);
        true.into()
    }
    let mut handles: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into `handles`.
    unsafe { EnumWindows(Some(add), LPARAM(&mut handles as *mut _ as isize)) }?;
    let own = if std::env::var("P2C_TEST_OWN_WINDOWS").as_deref() == Ok("1") {
        None
    } else {
        // SAFETY: plain query.
        Some(unsafe { GetCurrentProcessId() })
    };
    let mut exes: HashMap<u32, Option<(String, String)>> = HashMap::new();
    let mut out = Vec::new();
    for h in handles {
        // SAFETY: plain queries on a handle from EnumWindows (it may have
        // gone since; the calls then fail harmlessly).
        unsafe {
            if !IsWindowVisible(h).as_bool() {
                continue;
            }
            if GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
                continue;
            }
            let mut buf = [0u16; 512];
            let n = GetWindowTextW(h, &mut buf);
            let title = wide_to_string(&buf[..n.max(0) as usize]).trim().to_string();
            if title.is_empty() {
                continue;
            }
            let mut cls = [0u16; 256];
            let n = GetClassNameW(h, &mut cls);
            let class = wide_to_string(&cls[..n.max(0) as usize]);
            if SHELL_CLASSES.contains(&class.as_str()) {
                continue;
            }
            if cloaked(h) {
                continue;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if Some(pid) == own {
                continue;
            }
            let (exe, app) = exes
                .entry(pid)
                .or_insert_with(|| {
                    let path = exe_path(pid)?;
                    let p = Path::new(&path);
                    let exe = p.file_name()?.to_string_lossy().into_owned();
                    let stem = p
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| exe.clone());
                    let app = file_description(&path).unwrap_or(stem);
                    Some((exe, app))
                })
                .clone()
                .unwrap_or_default();
            let (width, height) = size(h);
            out.push(WindowInfo {
                id: h.0 as usize as u64,
                title,
                exe,
                app,
                pid,
                width,
                height,
                minimized: IsIconic(h).as_bool(),
            });
        }
    }
    Ok(out)
}

/// Closed (gone), minimized or fine.
pub fn status(id: u64) -> WindowStatus {
    let h = hwnd(id);
    // SAFETY: plain queries; a stale handle just reports "not a window".
    unsafe {
        if !IsWindow(Some(h)).as_bool() {
            WindowStatus::Closed
        } else if IsIconic(h).as_bool() {
            WindowStatus::Minimized
        } else {
            WindowStatus::Ok
        }
    }
}

/// The window's title as listed (trimmed); `None` once it is gone.
pub fn title(id: u64) -> Option<String> {
    let h = hwnd(id);
    // SAFETY: plain queries; a stale handle just reports "not a window".
    unsafe {
        if !IsWindow(Some(h)).as_bool() {
            return None;
        }
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(h, &mut buf);
        Some(wide_to_string(&buf[..n.max(0) as usize]).trim().to_string())
    }
}
