//! The `window` full-frame background: one application window captured into
//! the page (CPU slot and GPU page input) while Chromium's paints are paused,
//! and its process tree's sound mixed as page sound.
//!
//! Matching (privacy: a near miss must never put another window on camera):
//! while running, the capture follows the window handle it started with and
//! its title changes are followed (and saved by the app). Otherwise only a
//! window with the same exe and exactly the saved title starts, retried by
//! [`WindowShare::tick`].
//!
//! Threading: [`WindowBackend::start`] can block for seconds (the borderless
//! request, the audio client). [`WindowShare::sync`] starts at once (the
//! explicit `set_background` path accepts that stall); [`WindowShare::defer`]
//! only records the wish and the engine's own window thread starts it in
//! [`WindowShare::tick`]. The note lives behind its own lock, so the status
//! path never waits for a start.
use std::sync::Arc;

use p2c_core::{AudioRing, Frame};
use p2c_screen::{WindowInfo, WindowMatch, WindowStatus};
use parking_lot::Mutex;

pub use crate::screen::FrameOut;

/// Lists windows and starts captures (the real one, or a fake in tests).
pub trait WindowBackend: Send + Sync {
    fn list(&self) -> anyhow::Result<Vec<WindowInfo>>;
    /// Capture `w` (picture and sound); dropping the run stops both.
    fn start(&self, w: &WindowInfo) -> anyhow::Result<Box<dyn WindowRun>>;
    /// Show black once (the captured window closed). Default: nothing.
    fn black(&self) {}
}

/// One running window capture.
pub trait WindowRun: Send {
    fn status(&self) -> WindowStatus;
    /// The window's current title (`None`: unknown, e.g. gone).
    fn title(&self) -> Option<String>;
    /// Why the window's sound isn't captured, if it isn't.
    fn sound_error(&self) -> Option<String>;
}

/// Windows Graphics Capture (`p2c-screen`) plus WASAPI process loopback
/// (`p2c-audio`) into `ring`, mixed as page sound.
pub struct RealWindows {
    pub max_fps: u32,
    pub out: FrameOut,
    pub ring: AudioRing,
}

struct RealRun {
    // Dropped in this order: sound, then picture.
    sound: Option<p2c_audio::ProcessLoopback>,
    sound_failed: Option<String>,
    cap: p2c_screen::Capture,
    id: u64,
}

impl WindowRun for RealRun {
    fn status(&self) -> WindowStatus {
        if self.cap.error().is_some() {
            return WindowStatus::Closed;
        }
        self.cap.window_status()
    }
    fn title(&self) -> Option<String> {
        p2c_screen::window_title(self.id)
    }
    fn sound_error(&self) -> Option<String> {
        self.sound_failed
            .clone()
            .or_else(|| self.sound.as_ref().and_then(|s| s.error()))
    }
}

impl WindowBackend for RealWindows {
    fn list(&self) -> anyhow::Result<Vec<WindowInfo>> {
        p2c_screen::list_windows()
    }

    fn start(&self, w: &WindowInfo) -> anyhow::Result<Box<dyn WindowRun>> {
        let out = self.out.clone();
        let cap = p2c_screen::Capture::start_window(
            w.id,
            self.max_fps,
            Box::new(move |f| out(Arc::new(f))),
        )?;
        // A sound failure keeps the picture.
        let (sound, sound_failed) = match p2c_audio::ProcessLoopback::start(
            w.pid,
            self.ring.clone(),
        ) {
            Ok(s) => (Some(s), None),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), exe = %w.exe, "window sound unavailable");
                (None, Some(format!("{e:#}")))
            }
        };
        Ok(Box::new(RealRun {
            sound,
            sound_failed,
            cap,
            id: w.id,
        }))
    }

    fn black(&self) {
        (self.out)(Arc::new(Frame::new_black(64, 36)));
    }
}

/// The note for a match nothing exactly matches.
fn not_found(m: &WindowMatch) -> String {
    format!("window not found: {} ({}): choose it again", m.title, m.exe)
}

const MINIMIZED: &str = "window minimized: restore it (it can sit behind other windows)";
const CLOSED: &str = "window closed";
/// Titles of the running window remembered (so a `sync` with a title the
/// app hasn't caught up with yet is no restart).
const TITLES_KEPT: usize = 32;

struct Running {
    run: Box<dyn WindowRun>,
    id: u64,
    exe: String,
    app: String,
    /// Every title this run had, oldest first.
    titles: Vec<String>,
}

#[derive(Default)]
struct State {
    want: Option<WindowMatch>,
    running: Option<Running>,
    /// A start of this window failed: not retried by `tick`.
    failed_id: Option<u64>,
}

/// The running window capture, if any.
pub struct WindowShare {
    backend: Box<dyn WindowBackend>,
    state: Mutex<State>,
    note: Mutex<Option<String>>,
}

impl WindowShare {
    pub fn new(backend: Box<dyn WindowBackend>) -> Self {
        Self {
            backend,
            state: Mutex::new(State::default()),
            note: Mutex::new(None),
        }
    }

    pub fn list(&self) -> anyhow::Result<Vec<WindowInfo>> {
        self.backend.list()
    }

    /// The window an id or title text names (see `p2c_screen::resolve_text`),
    /// as the match to save. A window whose program can't be identified is
    /// refused: it could never be found again safely.
    pub fn resolve(&self, text: &str) -> anyhow::Result<WindowMatch> {
        let list = self.list()?;
        let w = p2c_screen::resolve_text(&list, text).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            !w.exe.is_empty(),
            "can't capture \"{}\": its program can't be identified (is it running as administrator?)",
            w.title
        );
        Ok(WindowMatch {
            exe: w.exe.clone(),
            title: w.title.clone(),
        })
    }

    /// Whether `m` is what is wanted already: the same match, or the running
    /// window under a title it had (the app may not have caught up yet).
    fn is_current(st: &State, m: &WindowMatch) -> bool {
        st.want.as_ref() == Some(m)
            || st
                .running
                .as_ref()
                .is_some_and(|r| r.exe.eq_ignore_ascii_case(&m.exe) && r.titles.contains(&m.title))
    }

    /// Record a new wish (stopping the old capture first).
    fn set_want(&self, st: &mut State, want: Option<&WindowMatch>) -> bool {
        match want {
            None => {
                st.running = None;
                st.want = None;
                st.failed_id = None;
                *self.note.lock() = None;
                false
            }
            Some(m) if Self::is_current(st, m) => false,
            Some(m) => {
                st.running = None;
                st.failed_id = None;
                st.want = Some(m.clone());
                *self.note.lock() = None;
                true
            }
        }
    }

    /// Capture `want` (`None`: stop), starting now if it is found (may block
    /// for seconds). The same wish again is no restart.
    pub fn sync(&self, want: Option<&WindowMatch>) {
        let mut st = self.state.lock();
        if self.set_want(&mut st, want) {
            self.try_start(&mut st);
        }
    }

    /// Record `want` without starting it (the next [`WindowShare::tick`]
    /// does, off the caller's thread).
    pub fn defer(&self, want: Option<&WindowMatch>) {
        let mut st = self.state.lock();
        self.set_want(&mut st, want);
    }

    /// Start the wanted window if exactly it is listed.
    fn try_start(&self, st: &mut State) {
        let Some(m) = st.want.clone() else { return };
        let note = |n: String| *self.note.lock() = Some(n);
        if m.exe.is_empty() {
            return note(not_found(&m));
        }
        let list = match self.backend.list() {
            Ok(l) => l,
            Err(e) => return note(format!("window capture failed: {e:#}")),
        };
        let Some(w) = p2c_screen::find_exact(&list, &m) else {
            return note(not_found(&m));
        };
        if st.failed_id == Some(w.id) {
            return; // its failure note stays; not retried
        }
        match self.backend.start(w) {
            Ok(run) => {
                tracing::info!(exe = %w.exe, "window background on");
                st.failed_id = None;
                st.running = Some(Running {
                    run,
                    id: w.id,
                    exe: w.exe.clone(),
                    app: w.app.clone(),
                    titles: vec![w.title.clone()],
                });
                self.update_note(st);
            }
            Err(e) => {
                let msg = format!("window capture failed: {e:#}");
                tracing::warn!("{msg}");
                st.failed_id = Some(w.id);
                note(msg);
            }
        }
    }

    /// The note while running: minimized, else a sound failure, else the
    /// browser warning.
    fn update_note(&self, st: &State) {
        let Some(r) = &st.running else { return };
        let n = if r.run.status() == WindowStatus::Minimized {
            Some(MINIMIZED.to_string())
        } else if let Some(e) = r.run.sound_error() {
            Some(format!("window sound unavailable: {e}"))
        } else if p2c_screen::is_browser(&r.exe) {
            Some(format!("Sound: everything playing in {}", r.app))
        } else {
            None
        };
        *self.note.lock() = n;
    }

    /// Every ~2 s: retry a match that wasn't found, follow the running
    /// window's state and title. Returns the new match when the title
    /// changed (the app saves it).
    pub fn tick(&self) -> Option<WindowMatch> {
        let mut st = self.state.lock();
        st.want.as_ref()?;
        let Some(r) = st.running.as_mut() else {
            self.try_start(&mut st);
            return None;
        };
        if r.run.status() == WindowStatus::Closed {
            tracing::info!(window = r.id, "captured window closed");
            st.running = None;
            self.backend.black();
            *self.note.lock() = Some(CLOSED.into());
            return None;
        }
        let changed = match r.run.title() {
            Some(t) if !t.is_empty() && r.titles.last() != Some(&t) => {
                r.titles.retain(|x| *x != t);
                r.titles.push(t.clone());
                if r.titles.len() > TITLES_KEPT {
                    r.titles.remove(0);
                }
                let want = st.want.as_mut().expect("checked above");
                want.title = t;
                Some(want.clone())
            }
            _ => None,
        };
        self.update_note(&st);
        changed
    }

    /// Why the window isn't showing, or a warning about its sound.
    pub fn note(&self) -> Option<String> {
        self.note.lock().clone()
    }

    /// The window being captured.
    #[cfg(test)]
    pub fn running(&self) -> Option<u64> {
        self.state.lock().running.as_ref().map(|r| r.id)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    pub struct RunState {
        pub status: Option<WindowStatus>,
        pub title: Option<String>,
        pub sound_error: Option<String>,
    }

    /// Scripted window list; counts starts, live runs and black frames.
    #[derive(Default, Clone)]
    pub struct FakeWindows {
        pub list: Arc<Mutex<Vec<WindowInfo>>>,
        pub starts: Arc<Mutex<Vec<u64>>>,
        pub live: Arc<AtomicUsize>,
        pub blacks: Arc<AtomicUsize>,
        pub fail: Arc<Mutex<Option<String>>>,
        pub run: Arc<Mutex<RunState>>,
        pub unsupported: bool,
    }

    struct FakeRun {
        live: Arc<AtomicUsize>,
        st: Arc<Mutex<RunState>>,
    }
    impl Drop for FakeRun {
        fn drop(&mut self) {
            self.live.fetch_sub(1, Ordering::SeqCst);
        }
    }
    impl WindowRun for FakeRun {
        fn status(&self) -> WindowStatus {
            self.st.lock().status.unwrap_or(WindowStatus::Ok)
        }
        fn title(&self) -> Option<String> {
            self.st.lock().title.clone()
        }
        fn sound_error(&self) -> Option<String> {
            self.st.lock().sound_error.clone()
        }
    }

    impl WindowBackend for FakeWindows {
        fn list(&self) -> anyhow::Result<Vec<WindowInfo>> {
            anyhow::ensure!(!self.unsupported, p2c_screen::WINDOW_UNSUPPORTED);
            Ok(self.list.lock().clone())
        }
        fn start(&self, w: &WindowInfo) -> anyhow::Result<Box<dyn WindowRun>> {
            if let Some(e) = self.fail.lock().clone() {
                anyhow::bail!(e);
            }
            self.starts.lock().push(w.id);
            self.live.fetch_add(1, Ordering::SeqCst);
            *self.run.lock() = RunState {
                title: Some(w.title.clone()),
                ..Default::default()
            };
            Ok(Box::new(FakeRun {
                live: self.live.clone(),
                st: self.run.clone(),
            }))
        }
        fn black(&self) {
            self.blacks.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn win(id: u64, exe: &str, title: &str) -> WindowInfo {
        WindowInfo {
            id,
            title: title.into(),
            exe: exe.into(),
            app: match exe {
                "msedge.exe" => "Microsoft Edge".into(),
                "chrome.exe" => "Google Chrome".into(),
                e => e.trim_end_matches(".exe").into(),
            },
            pid: 100 + id as u32,
            width: 1280,
            height: 720,
            minimized: false,
        }
    }

    pub fn m(exe: &str, title: &str) -> WindowMatch {
        WindowMatch {
            exe: exe.into(),
            title: title.into(),
        }
    }

    const CHROME_SOUND: &str = "Sound: everything playing in Google Chrome";

    fn share() -> (FakeWindows, WindowShare) {
        let f = FakeWindows::default();
        (f.clone(), WindowShare::new(Box::new(f)))
    }

    #[test]
    fn exact_match_starts_and_near_miss_does_not() {
        let (f, s) = share();
        *f.list.lock() = vec![win(1, "chrome.exe", "Gmail")];
        s.sync(Some(&m("chrome.exe", "A - Twitch")));
        assert!(f.starts.lock().is_empty(), "never a near miss");
        assert_eq!(s.running(), None);
        assert_eq!(
            s.note().as_deref(),
            Some("window not found: A - Twitch (chrome.exe): choose it again")
        );
        f.list.lock().push(win(2, "chrome.exe", "A - Twitch"));
        assert_eq!(s.tick(), None);
        assert_eq!(*f.starts.lock(), vec![2]);
        assert_eq!(s.running(), Some(2));
        assert_eq!(
            s.note().as_deref(),
            Some(CHROME_SOUND),
            "browser warning only"
        );
        // The same wish again: no restart. None: stopped.
        s.sync(Some(&m("chrome.exe", "A - Twitch")));
        assert_eq!(f.live.load(Ordering::SeqCst), 1);
        s.sync(None);
        assert_eq!(f.live.load(Ordering::SeqCst), 0);
        assert_eq!(s.note(), None);
        assert_eq!(s.tick(), None);
        assert_eq!(f.starts.lock().len(), 1, "nothing wanted: nothing started");
    }

    #[test]
    fn title_change_while_running_is_followed_and_saved() {
        let (f, s) = share();
        *f.list.lock() = vec![win(2, "chrome.exe", "A - Twitch")];
        s.sync(Some(&m("chrome.exe", "A - Twitch")));
        assert_eq!(s.tick(), None, "unchanged title");
        f.run.lock().title = Some("B - Twitch".into());
        assert_eq!(s.tick(), Some(m("chrome.exe", "B - Twitch")));
        assert_eq!(s.tick(), None, "reported once");
        assert_eq!(*f.starts.lock(), vec![2], "not restarted");
        // The app may still hold the old title for a moment: no restart.
        s.sync(Some(&m("chrome.exe", "A - Twitch")));
        s.sync(Some(&m("chrome.exe", "B - Twitch")));
        assert_eq!(*f.starts.lock(), vec![2]);
        assert_eq!(f.live.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn minimized_then_closed() {
        let (f, s) = share();
        *f.list.lock() = vec![win(2, "chrome.exe", "A - Twitch")];
        s.sync(Some(&m("chrome.exe", "A - Twitch")));
        f.run.lock().status = Some(WindowStatus::Minimized);
        s.tick();
        assert_eq!(
            s.note().as_deref(),
            Some("window minimized: restore it (it can sit behind other windows)")
        );
        f.run.lock().status = Some(WindowStatus::Ok);
        f.run.lock().title = Some("B - Twitch".into());
        assert_eq!(s.tick(), Some(m("chrome.exe", "B - Twitch")));
        assert_eq!(s.note().as_deref(), Some(CHROME_SOUND));
        f.run.lock().status = Some(WindowStatus::Closed);
        s.tick();
        assert_eq!(s.note().as_deref(), Some("window closed"));
        assert_eq!(f.live.load(Ordering::SeqCst), 0, "run dropped");
        assert_eq!(f.blacks.load(Ordering::SeqCst), 1);
        // The old title doesn't resume; the new (saved) one does.
        *f.list.lock() = vec![win(3, "chrome.exe", "A - Twitch")];
        s.tick();
        assert_eq!(s.running(), None);
        assert_eq!(
            s.note().as_deref(),
            Some("window not found: B - Twitch (chrome.exe): choose it again")
        );
        assert_eq!(f.blacks.load(Ordering::SeqCst), 1, "black once");
        f.list.lock().push(win(4, "chrome.exe", "B - Twitch"));
        s.tick();
        assert_eq!(s.running(), Some(4));
        assert_eq!(*f.starts.lock(), vec![2, 4]);
    }

    #[test]
    fn start_failure_and_sound_failure_are_notes() {
        let (f, s) = share();
        *f.list.lock() = vec![win(2, "game.exe", "Game")];
        *f.fail.lock() = Some("access denied".into());
        s.sync(Some(&m("game.exe", "Game")));
        assert_eq!(
            s.note().as_deref(),
            Some("window capture failed: access denied")
        );
        assert_eq!(s.running(), None);
        s.tick(); // the same window isn't retried
        assert_eq!(
            s.note().as_deref(),
            Some("window capture failed: access denied")
        );
        *f.fail.lock() = None;
        s.sync(None);
        s.sync(Some(&m("game.exe", "Game")));
        assert_eq!(s.running(), Some(2));
        f.run.lock().sound_error = Some("no audio device".into());
        s.tick();
        assert_eq!(
            s.note().as_deref(),
            Some("window sound unavailable: no audio device")
        );
    }

    #[test]
    fn browser_sound_warning() {
        let (f, s) = share();
        *f.list.lock() = vec![win(7, "msedge.exe", "A - Twitch")];
        s.sync(Some(&m("msedge.exe", "A - Twitch")));
        assert_eq!(
            s.note().as_deref(),
            Some("Sound: everything playing in Microsoft Edge")
        );
        // A sound failure takes priority.
        f.run.lock().sound_error = Some("denied".into());
        s.tick();
        assert_eq!(
            s.note().as_deref(),
            Some("window sound unavailable: denied")
        );
    }

    #[test]
    fn resolve_picks_one_and_refuses_unknown_programs() {
        let (f, s) = share();
        *f.list.lock() = vec![
            win(1, "chrome.exe", "Gmail"),
            win(2, "chrome.exe", "A - Twitch"),
            win(3, "", "Admin tool"),
        ];
        assert_eq!(s.resolve("twitch").unwrap(), m("chrome.exe", "A - Twitch"));
        assert_eq!(s.resolve("1").unwrap(), m("chrome.exe", "Gmail"));
        let e = s.resolve("a").unwrap_err().to_string();
        assert!(e.contains("matches several windows"), "{e}");
        let e = s.resolve("admin").unwrap_err().to_string();
        assert!(e.contains("can't be identified"), "{e}");
        // An empty exe is never started either.
        s.sync(Some(&m("", "Admin tool")));
        assert!(f.starts.lock().is_empty());
        let u = WindowShare::new(Box::new(FakeWindows {
            unsupported: true,
            ..Default::default()
        }));
        assert_eq!(
            u.resolve("x").unwrap_err().to_string(),
            p2c_screen::WINDOW_UNSUPPORTED
        );
    }

    #[test]
    fn defer_starts_on_the_next_tick() {
        let (f, s) = share();
        *f.list.lock() = vec![win(2, "chrome.exe", "A - Twitch")];
        s.defer(Some(&m("chrome.exe", "A - Twitch")));
        assert!(f.starts.lock().is_empty());
        s.sync(Some(&m("chrome.exe", "A - Twitch")));
        assert!(f.starts.lock().is_empty(), "same wish: left to tick");
        s.tick();
        assert_eq!(*f.starts.lock(), vec![2]);
    }
}
