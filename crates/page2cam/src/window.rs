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
    if m.exe.is_empty() {
        return NONE_CHOSEN.into();
    }
    format!("window not found: {} ({}): choose it again", m.title, m.exe)
}

const MINIMIZED: &str = "window minimized: restore it (it can sit behind other windows)";
const CLOSED: &str = "window closed";
/// A saved window without a program (e.g. a hand-edited config).
const NONE_CHOSEN: &str = "no window chosen for the window background: choose a window";
/// Titles of the running window remembered (so a `sync` with a title the
/// app hasn't caught up with yet is no restart).
const TITLES_KEPT: usize = 32;

/// A title change of the running window (`tick`): the match before and
/// after. The app applies it only while it still shows `old`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleChange {
    pub old: WindowMatch,
    pub new: WindowMatch,
}

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
    /// A black frame went out since the last run stopped (the page slot
    /// holds no window picture).
    blank: bool,
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

    /// The window an id or title text names (see `p2c_screen::resolve_text`):
    /// the match to save and the chosen window's id (pass it to
    /// [`WindowShare::sync`], so exactly that window starts). A window whose
    /// program can't be identified is refused: it could never be found
    /// again safely.
    pub fn resolve(&self, text: &str) -> anyhow::Result<(WindowMatch, u64)> {
        let list = self.list()?;
        let w = p2c_screen::resolve_text(&list, text).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            !w.exe.is_empty(),
            "can't capture \"{}\": its program can't be identified (is it running as administrator?)",
            w.title
        );
        Ok((
            WindowMatch {
                exe: w.exe.clone(),
                title: w.title.clone(),
            },
            w.id,
        ))
    }

    /// Whether the wish is what runs (or is wanted) already. With an id
    /// (an explicit choice): that very window runs. Without (the saved
    /// match): the same match, or the running window under a title it had
    /// (the app may not have caught up yet).
    fn is_current(st: &State, m: &WindowMatch, id: Option<u64>) -> bool {
        match id {
            Some(id) => st.running.as_ref().is_some_and(|r| r.id == id),
            None => {
                st.want.as_ref() == Some(m)
                    || st.running.as_ref().is_some_and(|r| {
                        r.exe.eq_ignore_ascii_case(&m.exe) && r.titles.contains(&m.title)
                    })
            }
        }
    }

    /// Show black once since the last run stopped (never the old window's
    /// last frame under another choice).
    fn blank(&self, st: &mut State) {
        if !st.blank {
            self.backend.black();
            st.blank = true;
        }
    }

    /// Record a new wish, stopping the old capture first. True when it
    /// changed (a start is due).
    fn set_want(&self, st: &mut State, want: Option<&WindowMatch>, id: Option<u64>) -> bool {
        match want {
            None => {
                // The page comes back (repainted by the app): no black.
                st.running = None;
                st.want = None;
                st.failed_id = None;
                *self.note.lock() = None;
                false
            }
            Some(m) if Self::is_current(st, m, id) => false,
            Some(m) => {
                if st.running.take().is_some() {
                    st.blank = false;
                }
                // An explicit choice retries a window that failed before.
                if id.is_some() {
                    st.failed_id = None;
                }
                st.want = Some(m.clone());
                *self.note.lock() = None;
                true
            }
        }
    }

    /// Capture `want` (`None`: stop), starting now if it is found (may block
    /// for seconds). `id` is the window the operator picked (from
    /// [`WindowShare::resolve`]): exactly that one starts. Without it only a
    /// single window with the same exe and exactly the title starts. The
    /// same wish again is no restart.
    pub fn sync(&self, want: Option<&WindowMatch>, id: Option<u64>) {
        let mut st = self.state.lock();
        if self.set_want(&mut st, want, id) {
            self.try_start(&mut st, id);
        }
    }

    /// Record `want` without starting it (the next [`WindowShare::tick`]
    /// does, off the caller's thread).
    pub fn defer(&self, want: Option<&WindowMatch>) {
        let mut st = self.state.lock();
        if self.set_want(&mut st, want, None) {
            self.blank(&mut st);
        }
    }

    /// Start the wanted window: the one with `id` (an explicit choice), else
    /// the only one listed with the same exe and exactly the title. Never a
    /// guess: anything else leaves black and a note.
    fn try_start(&self, st: &mut State, id: Option<u64>) {
        let Some(m) = st.want.clone() else { return };
        if m.exe.is_empty() {
            *self.note.lock() = Some(not_found(&m));
            return self.blank(st);
        }
        let list = match self.backend.list() {
            Ok(l) => l,
            Err(e) => {
                *self.note.lock() = Some(format!("window capture failed: {e:#}"));
                return self.blank(st);
            }
        };
        let found: Vec<&WindowInfo> = match id {
            Some(id) => list
                .iter()
                .filter(|w| w.id == id && w.exe.eq_ignore_ascii_case(&m.exe))
                .collect(),
            None => list
                .iter()
                .filter(|w| w.exe.eq_ignore_ascii_case(&m.exe) && w.title == m.title)
                .collect(),
        };
        let w = match found.as_slice() {
            [w] => *w,
            [] => {
                *self.note.lock() = Some(not_found(&m));
                return self.blank(st);
            }
            _ => {
                *self.note.lock() = Some(format!(
                    "several windows are called {} ({}): choose one again",
                    m.title, m.exe
                ));
                return self.blank(st);
            }
        };
        if st.failed_id == Some(w.id) {
            return self.blank(st); // its failure note stays; not retried
        }
        match self.backend.start(w) {
            Ok(run) => {
                tracing::info!(exe = %w.exe, window = w.id, "window background on");
                st.failed_id = None;
                st.blank = false;
                st.running = Some(Running {
                    run,
                    id: w.id,
                    exe: w.exe.clone(),
                    app: w.app.clone(),
                    titles: vec![w.title.clone()],
                });
                // An explicit pick by id may carry an older title.
                if let Some(want) = st.want.as_mut() {
                    want.title = w.title.clone();
                }
                self.update_note(st);
            }
            Err(e) => {
                let msg = format!("window capture failed: {e:#}");
                tracing::warn!("{msg}");
                st.failed_id = Some(w.id);
                *self.note.lock() = Some(msg);
                self.blank(st);
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
    /// window's state and title. Returns the old and new match when the
    /// title changed (the app saves it if it still shows the old one).
    pub fn tick(&self) -> Option<TitleChange> {
        let mut st = self.state.lock();
        st.want.as_ref()?;
        let Some(r) = st.running.as_mut() else {
            self.try_start(&mut st, None);
            return None;
        };
        if r.run.status() == WindowStatus::Closed {
            tracing::info!(window = r.id, "captured window closed");
            st.running = None;
            st.blank = false;
            self.blank(&mut st);
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
                let old = want.clone();
                want.title = t;
                Some(TitleChange {
                    old,
                    new: want.clone(),
                })
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
        s.sync(Some(&m("chrome.exe", "A - Twitch")), None);
        assert!(f.starts.lock().is_empty(), "never a near miss");
        assert_eq!(s.running(), None);
        assert_eq!(
            s.note().as_deref(),
            Some("window not found: A - Twitch (chrome.exe): choose it again")
        );
        f.list.lock().push(win(2, "chrome.exe", "A - Twitch"));
        assert_eq!(s.tick().map(|c| c.new), None);
        assert_eq!(*f.starts.lock(), vec![2]);
        assert_eq!(s.running(), Some(2));
        assert_eq!(
            s.note().as_deref(),
            Some(CHROME_SOUND),
            "browser warning only"
        );
        // The same wish again: no restart. None: stopped.
        s.sync(Some(&m("chrome.exe", "A - Twitch")), None);
        assert_eq!(f.live.load(Ordering::SeqCst), 1);
        s.sync(None, None);
        assert_eq!(f.live.load(Ordering::SeqCst), 0);
        assert_eq!(s.note(), None);
        assert_eq!(s.tick().map(|c| c.new), None);
        assert_eq!(f.starts.lock().len(), 1, "nothing wanted: nothing started");
    }

    #[test]
    fn title_change_while_running_is_followed_and_saved() {
        let (f, s) = share();
        *f.list.lock() = vec![win(2, "chrome.exe", "A - Twitch")];
        s.sync(Some(&m("chrome.exe", "A - Twitch")), None);
        assert_eq!(s.tick().map(|c| c.new), None, "unchanged title");
        f.run.lock().title = Some("B - Twitch".into());
        assert_eq!(s.tick().map(|c| c.new), Some(m("chrome.exe", "B - Twitch")));
        assert_eq!(s.tick().map(|c| c.new), None, "reported once");
        assert_eq!(*f.starts.lock(), vec![2], "not restarted");
        // The app may still hold the old title for a moment: no restart.
        s.sync(Some(&m("chrome.exe", "A - Twitch")), None);
        s.sync(Some(&m("chrome.exe", "B - Twitch")), None);
        assert_eq!(*f.starts.lock(), vec![2]);
        assert_eq!(f.live.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn minimized_then_closed() {
        let (f, s) = share();
        *f.list.lock() = vec![win(2, "chrome.exe", "A - Twitch")];
        s.sync(Some(&m("chrome.exe", "A - Twitch")), None);
        f.run.lock().status = Some(WindowStatus::Minimized);
        s.tick();
        assert_eq!(
            s.note().as_deref(),
            Some("window minimized: restore it (it can sit behind other windows)")
        );
        f.run.lock().status = Some(WindowStatus::Ok);
        f.run.lock().title = Some("B - Twitch".into());
        assert_eq!(s.tick().map(|c| c.new), Some(m("chrome.exe", "B - Twitch")));
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
        s.sync(Some(&m("game.exe", "Game")), None);
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
        s.sync(None, None);
        s.sync(Some(&m("game.exe", "Game")), None);
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
        s.sync(Some(&m("msedge.exe", "A - Twitch")), None);
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
        assert_eq!(
            s.resolve("twitch").unwrap(),
            (m("chrome.exe", "A - Twitch"), 2)
        );
        assert_eq!(s.resolve("1").unwrap(), (m("chrome.exe", "Gmail"), 1));
        let e = s.resolve("a").unwrap_err().to_string();
        assert!(e.contains("matches several windows"), "{e}");
        let e = s.resolve("admin").unwrap_err().to_string();
        assert!(e.contains("can't be identified"), "{e}");
        // An empty exe is never started either, and the note says why.
        s.sync(Some(&m("", "Admin tool")), None);
        assert!(f.starts.lock().is_empty());
        assert_eq!(
            s.note().as_deref(),
            Some("no window chosen for the window background: choose a window")
        );
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
        s.sync(Some(&m("chrome.exe", "A - Twitch")), None);
        assert!(f.starts.lock().is_empty(), "same wish: left to tick");
        s.tick();
        assert_eq!(*f.starts.lock(), vec![2]);
    }

    /// Fix 1: the picked window starts, by id, even when another one has the
    /// same exe and title; resuming by match never guesses between them.
    #[test]
    fn the_picked_window_starts_and_twins_are_never_guessed() {
        let (f, s) = share();
        *f.list.lock() = vec![
            win(1, "chrome.exe", "New Tab"),
            win(2, "chrome.exe", "New Tab"),
        ];
        let (want, id) = s.resolve("2").unwrap();
        s.sync(Some(&want), Some(id));
        assert_eq!(*f.starts.lock(), vec![2], "the second one, as picked");
        // Same pick again: no restart. The first one picked: switched.
        s.sync(Some(&want), Some(2));
        assert_eq!(f.starts.lock().len(), 1);
        s.sync(Some(&want), Some(1));
        assert_eq!(*f.starts.lock(), vec![2, 1]);
        assert_eq!(f.live.load(Ordering::SeqCst), 1);
        // Another window of the same exe now carrying an old title of the
        // running one: picked by id, it replaces it.
        f.run.lock().title = Some("Docs".into());
        s.tick();
        f.list.lock()[0].title = "Docs".into();
        f.list.lock()[1].title = "New Tab".into();
        s.sync(Some(&m("chrome.exe", "New Tab")), Some(2));
        assert_eq!(s.running(), Some(2));
        // Resuming by match with two exact twins: nothing starts.
        s.sync(None, None);
        *f.list.lock() = vec![
            win(5, "chrome.exe", "New Tab"),
            win(6, "chrome.exe", "New Tab"),
        ];
        s.sync(Some(&m("chrome.exe", "New Tab")), None);
        assert_eq!(s.running(), None);
        assert_eq!(
            s.note().as_deref(),
            Some("several windows are called New Tab (chrome.exe): choose one again")
        );
        s.tick();
        assert_eq!(s.running(), None, "never a guess");
    }

    /// Fix 2: no old window picture stays up: a switch to a window that
    /// isn't there, a failed start and a first choice that isn't found all
    /// show black.
    #[test]
    fn black_whenever_no_window_replaces_the_last_one() {
        let (f, s) = share();
        let blacks = || f.blacks.load(Ordering::SeqCst);
        *f.list.lock() = vec![win(1, "app.exe", "A")];
        s.sync(Some(&m("app.exe", "Missing")), None);
        assert_eq!(blacks(), 1, "first choice not found");
        s.tick();
        assert_eq!(blacks(), 1, "once");
        s.sync(Some(&m("app.exe", "A")), Some(1));
        assert_eq!(s.running(), Some(1));
        s.sync(Some(&m("app.exe", "B")), None);
        assert_eq!(
            (s.running(), blacks()),
            (None, 2),
            "switch to a missing one"
        );
        s.sync(Some(&m("app.exe", "A")), Some(1));
        *f.fail.lock() = Some("access denied".into());
        f.list.lock().push(win(2, "app.exe", "C"));
        s.sync(Some(&m("app.exe", "C")), Some(2));
        assert_eq!((s.running(), blacks()), (None, 3), "failed start");
        // Stopping (back to the page) needs no black: the page repaints.
        s.sync(None, None);
        assert_eq!(blacks(), 3);
    }

    /// Fix 4: picking a window again after its start failed retries it.
    #[test]
    fn picking_a_failed_window_again_retries_it() {
        let (f, s) = share();
        *f.list.lock() = vec![win(2, "game.exe", "Game")];
        *f.fail.lock() = Some("access denied".into());
        s.sync(Some(&m("game.exe", "Game")), Some(2));
        assert_eq!(s.running(), None);
        *f.fail.lock() = None;
        s.tick();
        assert_eq!(s.running(), None, "tick doesn't retry it");
        s.sync(Some(&m("game.exe", "Game")), None);
        assert_eq!(s.running(), None, "the saved wish unchanged: no retry");
        s.sync(Some(&m("game.exe", "Game")), Some(2));
        assert_eq!(s.running(), Some(2), "picked again: retried");
        assert_eq!(s.note(), None);
    }

    /// Fix 3: a title change names the match it replaces.
    #[test]
    fn title_change_names_the_old_match() {
        let (f, s) = share();
        *f.list.lock() = vec![win(2, "chrome.exe", "A")];
        s.sync(Some(&m("chrome.exe", "A")), Some(2));
        f.run.lock().title = Some("B".into());
        assert_eq!(
            s.tick(),
            Some(TitleChange {
                old: m("chrome.exe", "A"),
                new: m("chrome.exe", "B"),
            })
        );
    }
}
