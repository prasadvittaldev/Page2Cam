//! Screen capture: one monitor (the `screen` full-frame background) or one
//! application window (the `window` background) as BGRA frames.
//! Windows 11: Windows Graphics Capture. Other platforms: [`list`],
//! [`Capture::start`] fail with [`UNSUPPORTED`], window capture with
//! [`WINDOW_UNSUPPORTED`].
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use p2c_core::Frame;

#[cfg(windows)]
mod win;
#[cfg(windows)]
mod win_window;
mod window;

pub use window::{
    find_exact, is_browser, list_windows, resolve_text, WindowInfo, WindowMatch, WindowStatus,
    WINDOW_UNSUPPORTED,
};

/// The error on platforms without screen capture.
pub const UNSUPPORTED: &str = "screen capture is Windows-only for now";

/// One monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenInfo {
    /// Stable id: the display device name without `\\.\` (e.g. `DISPLAY1`).
    pub id: String,
    /// 1-based position in the system's list.
    pub index: u32,
    /// "Screen <index>".
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// The monitors, primary first is not guaranteed (system order).
pub fn list() -> anyhow::Result<Vec<ScreenInfo>> {
    #[cfg(windows)]
    {
        win::list()
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!(UNSUPPORTED)
    }
}

/// `\\.\DISPLAY1` → `DISPLAY1`.
pub fn id_from_device(device: &str) -> String {
    device.trim_start_matches(r"\\.\").to_string()
}

/// The monitor for a saved/requested id: empty or `primary` → the primary
/// one; otherwise the id (case-insensitive) or a 1-based index.
pub fn pick<'a>(screens: &'a [ScreenInfo], id: &str) -> Option<&'a ScreenInfo> {
    let id = id.trim();
    if id.is_empty() || id.eq_ignore_ascii_case("primary") {
        return screens.iter().find(|s| s.primary).or(screens.first());
    }
    screens
        .iter()
        .find(|s| s.id.eq_ignore_ascii_case(id))
        .or_else(|| {
            let n: u32 = id.parse().ok()?;
            screens.iter().find(|s| s.index == n)
        })
}

/// Copy `h` rows of `w` BGRA pixels out of a mapped surface with
/// `row_pitch` bytes per row, alpha forced opaque (the desktop's alpha is
/// not meaningful).
pub fn copy_rows(src: &[u8], row_pitch: usize, w: u32, h: u32) -> Option<Vec<u8>> {
    let row = w as usize * 4;
    if row_pitch < row || h == 0 || src.len() < row_pitch * (h as usize - 1) + row {
        return None;
    }
    let mut out = Vec::with_capacity(row * h as usize);
    for y in 0..h as usize {
        out.extend_from_slice(&src[y * row_pitch..y * row_pitch + row]);
    }
    for a in out.iter_mut().skip(3).step_by(4) {
        *a = 255;
    }
    Some(out)
}

/// A running capture of one monitor or one window. Frames go to the sink on
/// the capture thread, at most `max_fps` a second and only when the content
/// changed. Dropping it stops the capture.
pub struct Capture {
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
    target: Target,
}

enum Target {
    Screen(ScreenInfo),
    /// HWND, and set when Windows reports the capture item closed.
    #[cfg_attr(not(windows), allow(dead_code))]
    Window {
        id: u64,
        closed: Arc<AtomicBool>,
    },
}

/// Receives captured frames (on the capture thread).
pub type Sink = Box<dyn FnMut(Frame) + Send>;

fn interval(max_fps: u32) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(max_fps.max(1)))
}

impl Capture {
    /// Start capturing the monitor `id` (see [`pick`]). Fails at once when
    /// the monitor is unknown or capture can't start.
    pub fn start(id: &str, max_fps: u32, sink: Sink) -> anyhow::Result<Capture> {
        let screens = list()?;
        let screen = pick(&screens, id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no screen {id:?}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        #[cfg(windows)]
        let thread = win::spawn(
            win::Source::Monitor(screen.id.clone()),
            interval(max_fps),
            sink,
            stop.clone(),
            error.clone(),
        )?;
        #[cfg(not(windows))]
        let thread = {
            let _ = (interval(max_fps), sink);
            anyhow::bail!(UNSUPPORTED)
        };
        tracing::info!(screen = %screen.id, w = screen.width, h = screen.height, "screen capture on");
        Ok(Capture {
            stop,
            error,
            thread: Some(thread),
            target: Target::Screen(screen),
        })
    }

    /// Start capturing the window with this handle (see [`list_windows`]):
    /// cursor off, borderless where Windows allows it, the frame size
    /// following the window; no frames while it is minimized.
    pub fn start_window(id: u64, max_fps: u32, sink: Sink) -> anyhow::Result<Capture> {
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let closed = Arc::new(AtomicBool::new(false));
        #[cfg(windows)]
        let thread = win::spawn(
            win::Source::Window {
                id,
                closed: closed.clone(),
            },
            interval(max_fps),
            sink,
            stop.clone(),
            error.clone(),
        )?;
        #[cfg(not(windows))]
        let thread = {
            let _ = (interval(max_fps), sink);
            anyhow::bail!(WINDOW_UNSUPPORTED)
        };
        tracing::info!(window = id, "window capture on");
        Ok(Capture {
            stop,
            error,
            thread: Some(thread),
            target: Target::Window { id, closed },
        })
    }

    /// The monitor being captured (none for a window).
    pub fn screen(&self) -> Option<&ScreenInfo> {
        match &self.target {
            Target::Screen(s) => Some(s),
            Target::Window { .. } => None,
        }
    }

    /// The captured window's state (always `Ok` for a monitor): `Closed`
    /// once the window is gone or Windows closed the capture item,
    /// `Minimized` while it is minimized (no frames then).
    pub fn window_status(&self) -> WindowStatus {
        match &self.target {
            Target::Screen(_) => WindowStatus::Ok,
            Target::Window { id, closed } => {
                if closed.load(Ordering::SeqCst) {
                    return WindowStatus::Closed;
                }
                #[cfg(windows)]
                {
                    win_window::status(*id)
                }
                #[cfg(not(windows))]
                {
                    let _ = id;
                    WindowStatus::Closed
                }
            }
        }
    }

    /// Why capture stopped by itself (e.g. the monitor went away).
    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        match &self.target {
            Target::Screen(s) => tracing::info!(screen = %s.id, "screen capture off"),
            Target::Window { id, .. } => tracing::info!(window = id, "window capture off"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str, index: u32, primary: bool) -> ScreenInfo {
        ScreenInfo {
            id: id.into(),
            index,
            name: format!("Screen {index}"),
            width: 1920,
            height: 1080,
            primary,
        }
    }

    #[test]
    fn device_names_become_ids() {
        assert_eq!(id_from_device(r"\\.\DISPLAY1"), "DISPLAY1");
        assert_eq!(id_from_device("DISPLAY2"), "DISPLAY2");
    }

    #[test]
    fn pick_by_primary_id_or_index() {
        let l = [s("DISPLAY2", 1, false), s("DISPLAY1", 2, true)];
        assert_eq!(pick(&l, "").unwrap().id, "DISPLAY1");
        assert_eq!(pick(&l, "Primary").unwrap().id, "DISPLAY1");
        assert_eq!(pick(&l, "display2").unwrap().id, "DISPLAY2");
        assert_eq!(pick(&l, "2").unwrap().id, "DISPLAY1");
        assert!(pick(&l, "DISPLAY9").is_none());
        assert!(pick(&l, "9").is_none());
        assert!(pick(&[], "").is_none());
        // No primary flagged: the first.
        assert_eq!(pick(&[s("A", 1, false)], "").unwrap().id, "A");
    }

    #[test]
    fn rows_are_copied_without_padding_and_made_opaque() {
        // 2x2 pixels, row pitch 12 bytes (4 bytes padding).
        let src: Vec<u8> = (0..24).map(|i| i as u8).collect();
        let out = copy_rows(&src, 12, 2, 2).unwrap();
        assert_eq!(
            out,
            vec![0, 1, 2, 255, 4, 5, 6, 255, 12, 13, 14, 255, 16, 17, 18, 255]
        );
        // The last row needs no padding after it.
        assert!(copy_rows(&src[..20], 12, 2, 2).is_some());
        assert!(copy_rows(&src[..19], 12, 2, 2).is_none());
        assert!(copy_rows(&src, 4, 2, 2).is_none(), "pitch below the row");
        assert!(copy_rows(&src, 12, 2, 0).is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn other_platforms_say_unsupported() {
        assert_eq!(list().unwrap_err().to_string(), UNSUPPORTED);
        let e = Capture::start("", 30, Box::new(|_| {})).err().unwrap();
        assert_eq!(e.to_string(), UNSUPPORTED);
        let e = Capture::start_window(1, 30, Box::new(|_| {}))
            .err()
            .unwrap();
        assert_eq!(e.to_string(), WINDOW_UNSUPPORTED);
    }
}
