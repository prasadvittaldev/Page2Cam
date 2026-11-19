//! The Windows tray shell: a hidden window owning the notification-area
//! icon (plain Win32 `Shell_NotifyIconW`; balloons need `NIF_INFO`, which
//! the `tray-icon` crate does not offer), its popup menu and balloons. The
//! work happens on the controller thread ([`control`]); this thread only
//! pumps messages, so the icon and menu never wait for the engine.

pub mod autostart;
mod camera;
mod control;
mod engine;
mod icon;
pub mod job;
mod menu;
mod sysload;
mod update;

use std::cell::RefCell;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use anyhow::Context;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO,
    NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NOTIFYICONDATAW,
    NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    FindWindowW, GetMessageW, KillTimer, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterWindowMessageW, SetTimer, TranslateMessage, MSG, WINDOW_EX_STYLE, WM_APP, WM_CLOSE,
    WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_QUERYENDSESSION, WM_TIMER, WNDCLASSEXW,
    WS_OVERLAPPED,
};

use crate::health::Level;
use crate::menu::MenuId;
use crate::single::{claim_single_instance, Instance};
use control::{Cmd, Ui};

const CLASS: &str = "Page2CamTrayWindow";
/// Registered message a second launch posts to the running tray.
const SHOW_MSG: &str = "Page2Cam.Tray.Show";
const WM_TRAY: u32 = WM_APP + 1;
const WM_REFRESH: u32 = WM_APP + 2;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const ICON_ID: u32 = 1;
/// Balloon notifications (NOTIFYICON_VERSION_4 events in LOWORD(lParam)).
const NIN_BALLOONHIDE: u32 = 0x0403;
const NIN_BALLOONTIMEOUT: u32 = 0x0404;
const NIN_BALLOONUSERCLICK: u32 = 0x0405;
const BALLOON_TIMER: usize = 1;
const BALLOON_MAX_MS: u32 = 10_000;
/// How long sign-out waits for the tray to end the engine and remove the
/// camera (it needs about 1 s at worst).
const END_SESSION_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Window-thread state (the window procedure has no other context).
struct Wnd {
    tx: Sender<Cmd>,
    ui: Arc<Ui>,
    icons: icon::Icons,
    level: Option<Level>,
    tooltip: String,
    taskbar_created: u32,
    show_msg: u32,
    /// Balloons waiting for the one on screen to go.
    balloons: Vec<crate::notify::Balloon>,
    balloon_showing: bool,
}

thread_local! {
    static WND: RefCell<Option<Wnd>> = const { RefCell::new(None) };
}

/// Run the tray until Quit. A second tray only signals the first and
/// returns `Ok(())`.
pub fn run() -> anyhow::Result<()> {
    let _lock = match claim_single_instance(p2c_proc::TRAY_LOCK, signal_running_tray) {
        Instance::First(l) => l,
        Instance::Other => {
            tracing::info!("the tray is already running; asked it to show itself");
            return Ok(());
        }
    };
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        "Page2Cam tray starting"
    );
    // SAFETY: standard window creation and message loop on this thread.
    unsafe {
        let hinst = GetModuleHandleW(PCWSTR::null()).context("GetModuleHandleW")?;
        let class = HSTRING::from(CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        anyhow::ensure!(
            RegisterClassExW(&wc) != 0,
            "registering the tray window class: {}",
            windows::core::Error::from_thread()
        );
        // A plain hidden top-level window (not message-only): it must get
        // the "TaskbarCreated" broadcast and be found by a second launch.
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            &class,
            &HSTRING::from("Page2Cam"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .context("creating the tray window")?;
        let (tx, rx) = std::sync::mpsc::channel();
        let ui = Arc::new(Ui::new(hwnd.0 as isize, WM_REFRESH));
        WND.with(|w| {
            *w.borrow_mut() = Some(Wnd {
                tx: tx.clone(),
                ui: ui.clone(),
                icons: icon::Icons::new(),
                level: None,
                tooltip: String::new(),
                taskbar_created: RegisterWindowMessageW(&HSTRING::from("TaskbarCreated")),
                show_msg: RegisterWindowMessageW(&HSTRING::from(SHOW_MSG)),
                balloons: Vec::new(),
                balloon_showing: false,
            })
        });
        add_icon(hwnd);
        let ui2 = ui.clone();
        let controller = std::thread::Builder::new()
            .name("p2c-tray-ctl".into())
            .spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    control::run(rx, tx, ui2.clone())
                }));
                if r.is_err() {
                    // The controller's drop already ended the engine and
                    // removed the camera; close the icon too.
                    tracing::error!("the tray controller crashed");
                    ui2.state.lock().quit = true;
                    ui2.poke();
                }
                r.is_ok()
            })
            .context("starting the tray controller")?;
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        WND.with(|w| w.borrow_mut().take());
        let ok = controller.join().unwrap_or(false);
        anyhow::ensure!(
            ok,
            "the tray stopped after an internal error (see page2cam-tray.log)"
        );
    }
    tracing::info!("Page2Cam tray closed");
    Ok(())
}

/// Ask a running tray to show itself (second launch).
fn signal_running_tray() {
    // SAFETY: plain calls with valid strings.
    unsafe {
        if let Ok(h) = FindWindowW(&HSTRING::from(CLASS), PCWSTR::null()) {
            // Let the tray bring its menu or the browser to the front.
            let _ = AllowSetForegroundWindow(u32::MAX);
            let m = RegisterWindowMessageW(&HSTRING::from(SHOW_MSG));
            let _ = PostMessageW(Some(h), m, WPARAM(0), LPARAM(0));
        }
    }
}

fn copy_wide(dst: &mut [u16], s: &str) {
    let n = dst.len() - 1;
    let mut i = 0;
    for u in s.encode_utf16().take(n) {
        dst[i] = u;
        i += 1;
    }
    dst[i] = 0;
}

fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

/// Add the icon (at start and after Explorer restarted).
fn add_icon(hwnd: HWND) {
    WND.with(|w| {
        let Ok(mut w) = w.try_borrow_mut() else {
            return;
        };
        let Some(w) = w.as_mut() else { return };
        let (level, tip) = {
            let s = w.ui.state.lock();
            (s.level, s.tooltip.clone())
        };
        let mut d = nid(hwnd);
        d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = WM_TRAY;
        if let Some(h) = w.icons.get(level) {
            d.hIcon = h;
        }
        copy_wide(&mut d.szTip, &tip);
        // SAFETY: valid struct for the calls.
        unsafe {
            if !Shell_NotifyIconW(NIM_ADD, &d).as_bool() {
                tracing::warn!("could not add the tray icon (Explorer not ready?)");
            }
            d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
        }
        w.level = Some(level);
        w.tooltip = tip;
    });
}

/// Apply what the controller published.
fn refresh(hwnd: HWND) {
    let Some(ui) = WND.with(|w| w.borrow().as_ref().map(|w| w.ui.clone())) else {
        return;
    };
    let (level, tip, balloons, show_menu, quit) = {
        let mut s = ui.state.lock();
        (
            s.level,
            s.tooltip.clone(),
            std::mem::take(&mut s.balloons),
            std::mem::take(&mut s.show_menu),
            s.quit,
        )
    };
    if quit {
        // SAFETY: our window.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid(hwnd));
            let _ = DestroyWindow(hwnd);
        }
        return;
    }
    WND.with(|w| {
        let Ok(mut w) = w.try_borrow_mut() else {
            return;
        };
        let Some(w) = w.as_mut() else { return };
        if w.level != Some(level) || w.tooltip != tip {
            let mut d = nid(hwnd);
            d.uFlags = NIF_ICON | NIF_TIP | NIF_SHOWTIP;
            if let Some(h) = w.icons.get(level) {
                d.hIcon = h;
            }
            copy_wide(&mut d.szTip, &tip);
            // SAFETY: valid struct.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
            }
            w.level = Some(level);
            w.tooltip = tip;
        }
    });
    WND.with(|w| {
        if let Ok(mut w) = w.try_borrow_mut() {
            if let Some(w) = w.as_mut() {
                w.balloons.extend(balloons);
            }
        }
    });
    pump_balloons(hwnd);
    if show_menu {
        popup(hwnd, None);
    }
}

/// Show the menu; the choice goes to the controller.
fn popup(hwnd: HWND, at: Option<POINT>) {
    let Some((ui, tx)) = WND.with(|w| w.borrow().as_ref().map(|w| (w.ui.clone(), w.tx.clone())))
    else {
        return;
    };
    let entries = ui.state.lock().entries.clone();
    // No borrow held: the menu's modal loop re-enters the window procedure.
    if let Some(id) = menu::show(hwnd, &entries, at) {
        let _ = tx.send(Cmd::Menu(id));
    }
}

fn send(cmd: Cmd) {
    WND.with(|w| {
        if let Some(w) = w.borrow().as_ref() {
            let _ = w.tx.send(cmd);
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let (taskbar_created, show_msg) = WND.with(|w| {
        w.try_borrow()
            .ok()
            .and_then(|w| w.as_ref().map(|w| (w.taskbar_created, w.show_msg)))
            .unwrap_or((0, 0))
    });
    match msg {
        WM_TRAY => {
            // NOTIFYICON_VERSION_4: event in LOWORD(lParam), anchor in wParam.
            let event = (lp.0 as u32) & 0xffff;
            if matches!(event, WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT) {
                let at = POINT {
                    x: (wp.0 & 0xffff) as u16 as i16 as i32,
                    y: ((wp.0 >> 16) & 0xffff) as u16 as i16 as i32,
                };
                popup(hwnd, Some(at));
            } else if matches!(
                event,
                NIN_BALLOONHIDE | NIN_BALLOONTIMEOUT | NIN_BALLOONUSERCLICK
            ) {
                balloon_done(hwnd);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == BALLOON_TIMER => {
            balloon_done(hwnd);
            LRESULT(0)
        }
        // Sign-out or shutdown: agree, then close gracefully when it happens.
        WM_QUERYENDSESSION => {
            // From now on an engine exit is part of the sign-out (the engine
            // gets these messages too), never a crash to restart.
            send(Cmd::SessionEnding(true));
            LRESULT(1)
        }
        WM_ENDSESSION => {
            if wp.0 != 0 {
                end_session();
            } else {
                // Another app vetoed the sign-out: back to normal.
                send(Cmd::SessionEnding(false));
            }
            LRESULT(0)
        }
        WM_REFRESH => {
            refresh(hwnd);
            LRESULT(0)
        }
        // Closed from outside (e.g. taskkill without /f): quit properly.
        WM_CLOSE => {
            send(Cmd::Menu(MenuId::Quit));
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: plain call.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        m if m != 0 && m == taskbar_created => {
            add_icon(hwnd);
            LRESULT(0)
        }
        m if m != 0 && m == show_msg => {
            send(Cmd::Show);
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Show the queued balloons (merged into one) unless one is on screen.
/// Windows replaces a balloon that is still showing, so the next one waits
/// for the hide/timeout notification, or 10 s at most (Focus Assist may
/// swallow both).
fn pump_balloons(hwnd: HWND) {
    let next = WND.with(|w| {
        let Ok(mut w) = w.try_borrow_mut() else {
            return None;
        };
        let w = w.as_mut()?;
        if w.balloon_showing {
            return None;
        }
        let b = crate::notify::merge(&std::mem::take(&mut w.balloons))?;
        w.balloon_showing = true;
        Some(b)
    });
    let Some(b) = next else { return };
    tracing::info!(warning = b.warning, "balloon: {}", b.text);
    let mut d = nid(hwnd);
    d.uFlags = NIF_INFO;
    copy_wide(&mut d.szInfoTitle, "Page2Cam");
    copy_wide(&mut d.szInfo, &b.text);
    d.dwInfoFlags = if b.warning { NIIF_WARNING } else { NIIF_INFO };
    // SAFETY: valid struct; our window.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        SetTimer(Some(hwnd), BALLOON_TIMER, BALLOON_MAX_MS, None);
    }
}

/// The balloon on screen is gone (or assumed gone): show the next.
fn balloon_done(hwnd: HWND) {
    // SAFETY: our window and timer.
    unsafe {
        let _ = KillTimer(Some(hwnd), BALLOON_TIMER);
    }
    WND.with(|w| {
        if let Ok(mut w) = w.try_borrow_mut() {
            if let Some(w) = w.as_mut() {
                w.balloon_showing = false;
            }
        }
    });
    pump_balloons(hwnd);
}

/// Sign-out: ask the controller for a graceful stop and wait (bounded) for
/// it, since Windows ends the process once this message returns.
fn end_session() {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    send(Cmd::EndSession(tx));
    let begun = std::time::Instant::now();
    let _ = rx.recv_timeout(END_SESSION_WAIT);
    tracing::info!(
        waited_ms = begun.elapsed().as_millis() as u64,
        "session ending: tray closed"
    );
}
