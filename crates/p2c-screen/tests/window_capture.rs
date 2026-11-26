//! Real capture of our own Win32 window (Windows; `--ignored`).
#![cfg(windows)]
use std::sync::mpsc;
use std::time::{Duration, Instant};

use p2c_core::Frame;
use p2c_screen::{Capture, WindowStatus};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, PostMessageW,
    PostQuitMessage, RegisterClassW, SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW,
    CS_VREDRAW, HWND_TOP, MSG, SWP_NOMOVE, SWP_NOZORDER, SW_MINIMIZE, WINDOW_EX_STYLE, WM_APP,
    WM_DESTROY, WM_ERASEBKGND, WM_PAINT, WNDCLASSW, WS_POPUP, WS_VISIBLE,
};

const RESIZE: u32 = WM_APP;
const MINIMIZE: u32 = WM_APP + 1;
const DESTROY: u32 = WM_APP + 2;

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: plain Win32 calls on our own window, on its own thread.
    unsafe {
        match msg {
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                let brush = CreateSolidBrush(COLORREF(0x0000_FF00)); // pure green
                FillRect(dc, &ps.rcPaint, brush);
                let _ = DeleteObject(brush.into());
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            RESIZE => {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOP),
                    0,
                    0,
                    wp.0 as i32,
                    lp.0 as i32,
                    SWP_NOMOVE | SWP_NOZORDER,
                );
                LRESULT(0)
            }
            MINIMIZE => {
                let _ = ShowWindow(hwnd, SW_MINIMIZE);
                LRESULT(0)
            }
            DESTROY => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Our own green window on its own thread with a message loop; its handle.
fn green_window(title: String) -> (u64, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let t = std::thread::spawn(move || {
        let wide: Vec<u16> = title.encode_utf16().chain([0]).collect();
        // SAFETY: class registration and window creation for this thread.
        unsafe {
            let inst = GetModuleHandleW(None).unwrap();
            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(proc),
                hInstance: inst.into(),
                lpszClassName: w!("P2cWindowTest"),
                ..Default::default()
            };
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("P2cWindowTest"),
                PCWSTR(wide.as_ptr()),
                WS_POPUP | WS_VISIBLE,
                120,
                120,
                640,
                360,
                None,
                None,
                Some(inst.into()),
                None,
            )
            .unwrap();
            tx.send(hwnd.0 as u64).unwrap();
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    });
    (rx.recv().unwrap(), t)
}

fn post(id: u64, msg: u32, wp: usize, lp: isize) {
    // SAFETY: posting to our own window.
    unsafe { PostMessageW(Some(HWND(id as _)), msg, WPARAM(wp), LPARAM(lp)) }.unwrap();
}

fn wait_frame(rx: &mpsc::Receiver<Frame>, secs: u64, ok: impl Fn(&Frame) -> bool) -> Frame {
    let end = Instant::now() + Duration::from_secs(secs);
    let mut last = None;
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(f) if ok(&f) => return f,
            Ok(f) => last = Some((f.width, f.height)),
            Err(_) => break,
        }
    }
    panic!("no matching frame within {secs} s (last {last:?})");
}

fn wait_status(cap: &Capture, want: WindowStatus, secs: u64) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if cap.window_status() == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("status {:?}, wanted {want:?}", cap.window_status());
}

#[test]
#[ignore = "creates a real window and captures it"]
fn captures_our_own_window() {
    // SAFETY: process-wide DPI mode, set before any window exists.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    // SAFETY: set before any thread reads the environment.
    unsafe { std::env::set_var("P2C_TEST_OWN_WINDOWS", "1") };
    let title = format!("p2c-window-test-{}", std::process::id());
    let (id, thread) = green_window(title.clone());

    let list = p2c_screen::list_windows().expect("list");
    let me = list
        .iter()
        .find(|w| w.title == title)
        .unwrap_or_else(|| panic!("own window listed: {list:#?}"));
    assert_eq!(me.id, id);
    assert_eq!(me.pid, std::process::id());
    assert!(me.exe.to_ascii_lowercase().ends_with(".exe"), "{me:?}");
    assert!(!me.app.is_empty() && !me.minimized, "{me:?}");
    assert_eq!(p2c_screen::resolve_text(&list, &title).unwrap().id, id);

    let (tx, rx) = mpsc::channel();
    let cap = Capture::start_window(
        id,
        30,
        Box::new(move |f| {
            let _ = tx.send(f);
        }),
    )
    .expect("start_window");
    assert_eq!(cap.window_status(), WindowStatus::Ok);

    let f = wait_frame(&rx, 3, |f| {
        let p = (f.height as usize / 2 * f.width as usize + f.width as usize / 2) * 4;
        let px = &f.data[p..p + 4];
        px[0] <= 3 && px[1] >= 252 && px[2] <= 3
    });
    println!("first green frame {}x{}", f.width, f.height);

    post(id, RESIZE, 801, 457);
    let f = wait_frame(&rx, 3, |f| (f.width, f.height) == (801, 457));
    assert!(f.data.chunks(4).all(|p| p[3] == 255), "opaque");

    post(id, MINIMIZE, 0, 0);
    wait_status(&cap, WindowStatus::Minimized, 2);

    post(id, DESTROY, 0, 0);
    wait_status(&cap, WindowStatus::Closed, 2);
    thread.join().unwrap();

    let t = Instant::now();
    drop(cap);
    assert!(t.elapsed() < Duration::from_secs(2), "stops quickly");
}
