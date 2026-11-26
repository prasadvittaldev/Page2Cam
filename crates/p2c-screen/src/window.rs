//! Application windows (the `window` full-frame background): the listing,
//! exact matching after a restart, and an agent's title text. Platform-free;
//! the Windows enumeration is in `win_window.rs`.

/// The error on platforms without window capture.
pub const WINDOW_UNSUPPORTED: &str = "window capture is Windows-only for now";

/// One top-level window that can be captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInfo {
    /// The window handle (HWND); valid only while the window lives.
    pub id: u64,
    pub title: String,
    /// Executable file name, e.g. `chrome.exe`.
    pub exe: String,
    /// The executable's FileDescription, else its file stem.
    pub app: String,
    pub pid: u32,
    pub width: u32,
    pub height: u32,
    pub minimized: bool,
}

/// What is saved to find a window again: same exe, exactly this title.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowMatch {
    pub exe: String,
    pub title: String,
}

/// The state of a captured window, polled by the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowStatus {
    Ok,
    Minimized,
    Closed,
}

/// Exe names of browsers whose sound covers every tab.
const BROWSERS: [&str; 6] = [
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "brave.exe",
    "opera.exe",
    "vivaldi.exe",
];

/// Visible top-level windows that are not tool windows, not cloaked and have
/// a title; never our own process's windows, the desktop or the taskbar.
pub fn list_windows() -> anyhow::Result<Vec<WindowInfo>> {
    #[cfg(windows)]
    {
        crate::win_window::list()
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!(WINDOW_UNSUPPORTED)
    }
}

/// A window's current title (`None` once it is gone, and off Windows).
pub fn window_title(id: u64) -> Option<String> {
    #[cfg(windows)]
    {
        crate::win_window::title(id)
    }
    #[cfg(not(windows))]
    {
        let _ = id;
        None
    }
}

/// The window with the same exe (case-insensitive) and exactly the title.
/// Never a near miss: another window must not end up on camera.
pub fn find_exact<'a>(l: &'a [WindowInfo], m: &WindowMatch) -> Option<&'a WindowInfo> {
    l.iter()
        .find(|w| w.exe.eq_ignore_ascii_case(&m.exe) && w.title == m.title)
}

fn titles<'a>(ws: impl Iterator<Item = &'a WindowInfo>) -> String {
    let t: Vec<&str> = ws.map(|w| w.title.as_str()).collect();
    if t.is_empty() {
        "none".into()
    } else {
        t.join("; ")
    }
}

/// An agent's window text: an id (decimal HWND) in the list, else an exact
/// title, else a unique case-insensitive substring of a title.
pub fn resolve_text<'a>(l: &'a [WindowInfo], text: &str) -> Result<&'a WindowInfo, String> {
    let text = text.trim();
    if let Ok(id) = text.parse::<u64>() {
        if let Some(w) = l.iter().find(|w| w.id == id) {
            return Ok(w);
        }
        // A stale id must never fall through to substring matching (a near
        // miss could put another window on camera). Only a window whose
        // whole title is exactly this text may still match.
        let exact: Vec<&WindowInfo> = l.iter().filter(|w| w.title == text).collect();
        return match exact.as_slice() {
            [one] => Ok(one),
            _ => Err("that window is gone: choose it again".into()),
        };
    }
    let exact: Vec<&WindowInfo> = l.iter().filter(|w| w.title == text).collect();
    let found = if exact.is_empty() {
        let low = text.to_lowercase();
        if low.is_empty() {
            Vec::new()
        } else {
            l.iter()
                .filter(|w| w.title.to_lowercase().contains(&low))
                .collect()
        }
    } else {
        exact
    };
    match found.as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "no window matches \"{text}\" (windows: {})",
            titles(l.iter())
        )),
        many => Err(format!(
            "\"{text}\" matches several windows: {}",
            titles(many.iter().copied())
        )),
    }
}

/// Whether the exe is a known browser (its sound covers every tab).
pub fn is_browser(exe: &str) -> bool {
    BROWSERS.iter().any(|b| b.eq_ignore_ascii_case(exe))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(id: u64, exe: &str, title: &str) -> WindowInfo {
        WindowInfo {
            id,
            title: title.into(),
            exe: exe.into(),
            app: exe.trim_end_matches(".exe").into(),
            pid: 1,
            width: 1280,
            height: 720,
            minimized: false,
        }
    }

    #[test]
    fn exact_match_needs_exe_and_title() {
        let l = [
            w(1, "chrome.exe", "xQc - Twitch - Google Chrome"),
            w(2, "msedge.exe", "xQc - Twitch - Microsoft Edge"),
        ];
        let m = WindowMatch {
            exe: "CHROME.EXE".into(),
            title: "xQc - Twitch - Google Chrome".into(),
        };
        assert_eq!(find_exact(&l, &m).unwrap().id, 1);
        let near = WindowMatch {
            exe: "chrome.exe".into(),
            title: "xQc - Twitch".into(),
        };
        assert!(find_exact(&l, &near).is_none(), "never a near miss");
        let other_exe = WindowMatch {
            exe: "firefox.exe".into(),
            title: "xQc - Twitch - Google Chrome".into(),
        };
        assert!(find_exact(&l, &other_exe).is_none());
    }

    #[test]
    fn agent_text_must_pick_one() {
        let l = [
            w(1, "chrome.exe", "Gmail - Google Chrome"),
            w(2, "chrome.exe", "Twitch - Google Chrome"),
        ];
        assert_eq!(resolve_text(&l, "twitch").unwrap().id, 2);
        let e = resolve_text(&l, "google chrome").unwrap_err();
        assert!(e.contains("matches several windows"), "{e}");
        assert!(
            e.contains("Gmail - Google Chrome; Twitch - Google Chrome"),
            "{e}"
        );
        let e = resolve_text(&l, "kick").unwrap_err();
        assert!(e.starts_with("no window matches \"kick\""), "{e}");
        assert!(e.contains("(windows: Gmail - Google Chrome; Twitch - Google Chrome)"));
        assert_eq!(resolve_text(&l, "1").unwrap().id, 1);
        // An exact title wins over substrings of others.
        let l2 = [w(5, "a.exe", "Notes"), w(6, "b.exe", "Notes (old)")];
        assert_eq!(resolve_text(&l2, "Notes").unwrap().id, 5);
        assert!(resolve_text(&l2, "").is_err());
        assert!(resolve_text(&[], "x")
            .unwrap_err()
            .contains("(windows: none)"));
    }

    /// An id that is no longer listed never falls through to title
    /// matching ("7" must not pick "Invoice 1007"), except a window whose
    /// whole title is exactly that text.
    #[test]
    fn a_gone_id_is_not_a_title() {
        let l = [w(1, "excel.exe", "Invoice 1007"), w(2, "game.exe", "2048")];
        assert_eq!(
            resolve_text(&l, "7").unwrap_err(),
            "that window is gone: choose it again"
        );
        assert_eq!(resolve_text(&l, " 2048 ").unwrap().id, 2);
        assert_eq!(resolve_text(&l, "1").unwrap().id, 1);
    }

    #[test]
    fn browsers_are_known() {
        assert!(is_browser("MSEDGE.exe") && !is_browser("game.exe"));
        for b in BROWSERS {
            assert!(is_browser(b));
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn windows_only() {
        assert_eq!(list_windows().unwrap_err().to_string(), WINDOW_UNSUPPORTED);
    }
}
