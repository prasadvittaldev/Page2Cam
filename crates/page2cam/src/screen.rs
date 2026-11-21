//! The `screen` full-frame background: one monitor captured into the page
//! (CPU slot and GPU page input) while Chromium's paints are paused.
use std::sync::Arc;

use p2c_core::Frame;
use p2c_screen::ScreenInfo;
use parking_lot::Mutex;

/// Lists monitors and starts captures (the real one, or a fake in tests).
pub trait ScreenBackend: Send + Sync {
    fn list(&self) -> anyhow::Result<Vec<ScreenInfo>>;
    /// Start capturing `id` (an id from `list`); dropping the handle stops it.
    fn start(&self, id: &str) -> anyhow::Result<Box<dyn Send>>;
}

/// Where captured frames go: the CPU page slot and, in GPU mode, the page
/// input (through the holder, so device rebuilds are followed).
pub type FrameOut = Arc<dyn Fn(Arc<Frame>) + Send + Sync>;

/// Windows Graphics Capture (`p2c-screen`).
pub struct RealScreens {
    pub max_fps: u32,
    pub out: FrameOut,
}

impl ScreenBackend for RealScreens {
    fn list(&self) -> anyhow::Result<Vec<ScreenInfo>> {
        p2c_screen::list()
    }

    fn start(&self, id: &str) -> anyhow::Result<Box<dyn Send>> {
        let out = self.out.clone();
        let cap =
            p2c_screen::Capture::start(id, self.max_fps, Box::new(move |f| out(Arc::new(f))))?;
        Ok(Box::new(cap))
    }
}

/// The running capture, if any.
pub struct ScreenShare {
    backend: Box<dyn ScreenBackend>,
    running: Mutex<Option<(String, Box<dyn Send>)>>,
    error: Mutex<Option<String>>,
}

impl ScreenShare {
    pub fn new(backend: Box<dyn ScreenBackend>) -> Self {
        Self {
            backend,
            running: Mutex::new(None),
            error: Mutex::new(None),
        }
    }

    pub fn list(&self) -> anyhow::Result<Vec<ScreenInfo>> {
        self.backend.list()
    }

    /// The monitor id for a requested one (empty or `primary` → the primary
    /// monitor; an id or a 1-based index). Errors name the choices.
    pub fn resolve(&self, id: &str) -> anyhow::Result<String> {
        let list = self.list()?;
        match p2c_screen::pick(&list, id) {
            Some(s) => Ok(s.id.clone()),
            None => {
                let ids: Vec<&str> = list.iter().map(|s| s.id.as_str()).collect();
                anyhow::bail!("no screen {id:?} (screens: {})", ids.join(", "))
            }
        }
    }

    /// Capture `want` (`None`: stop). Restarts only when the monitor changes.
    /// A failure is kept for [`ScreenShare::error`] (the output then shows
    /// the black background page).
    pub fn sync(&self, want: Option<&str>) {
        let mut running = self.running.lock();
        match want {
            None => {
                *running = None;
                *self.error.lock() = None;
            }
            Some(id) if running.as_ref().is_some_and(|(r, _)| r == id) => {}
            Some(id) => {
                // The old capture stops before the new one starts.
                *running = None;
                match self.backend.start(id) {
                    Ok(h) => {
                        *running = Some((id.to_string(), h));
                        *self.error.lock() = None;
                    }
                    Err(e) => {
                        let msg = format!("screen capture failed: {e:#}");
                        tracing::warn!("{msg}");
                        *self.error.lock() = Some(msg);
                    }
                }
            }
        }
    }

    /// The monitor being captured.
    #[cfg(test)]
    pub fn running(&self) -> Option<String> {
        self.running.lock().as_ref().map(|(id, _)| id.clone())
    }

    /// Why the chosen screen isn't showing.
    pub fn error(&self) -> Option<String> {
        self.error.lock().clone()
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Two monitors; counts live captures; `fail` makes starts fail.
    #[derive(Default, Clone)]
    pub struct FakeScreens {
        pub live: Arc<AtomicUsize>,
        pub starts: Arc<Mutex<Vec<String>>>,
        pub fail: Arc<Mutex<bool>>,
        pub unsupported: bool,
    }

    struct Live(Arc<AtomicUsize>);
    impl Drop for Live {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl ScreenBackend for FakeScreens {
        fn list(&self) -> anyhow::Result<Vec<ScreenInfo>> {
            anyhow::ensure!(!self.unsupported, p2c_screen::UNSUPPORTED);
            let s = |id: &str, index, primary| ScreenInfo {
                id: id.into(),
                index,
                name: format!("Screen {index}"),
                width: 1920,
                height: 1080,
                primary,
            };
            Ok(vec![s("DISPLAY2", 1, false), s("DISPLAY1", 2, true)])
        }
        fn start(&self, id: &str) -> anyhow::Result<Box<dyn Send>> {
            anyhow::ensure!(!*self.fail.lock(), "monitor gone");
            self.starts.lock().push(id.into());
            self.live.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Live(self.live.clone())))
        }
    }

    #[test]
    fn resolves_primary_ids_and_indexes() {
        let s = ScreenShare::new(Box::new(FakeScreens::default()));
        assert_eq!(s.resolve("").unwrap(), "DISPLAY1");
        assert_eq!(s.resolve("primary").unwrap(), "DISPLAY1");
        assert_eq!(s.resolve("display2").unwrap(), "DISPLAY2");
        assert_eq!(s.resolve("1").unwrap(), "DISPLAY2");
        let e = s.resolve("DISPLAY7").unwrap_err().to_string();
        assert!(e.contains("DISPLAY2, DISPLAY1"), "{e}");
        let u = ScreenShare::new(Box::new(FakeScreens {
            unsupported: true,
            ..Default::default()
        }));
        assert_eq!(
            u.resolve("").unwrap_err().to_string(),
            p2c_screen::UNSUPPORTED
        );
    }

    #[test]
    fn sync_starts_switches_and_stops_one_capture() {
        let f = FakeScreens::default();
        let s = ScreenShare::new(Box::new(f.clone()));
        s.sync(Some("DISPLAY1"));
        s.sync(Some("DISPLAY1")); // unchanged: no restart
        assert_eq!(f.live.load(Ordering::SeqCst), 1);
        s.sync(Some("DISPLAY2"));
        assert_eq!(f.live.load(Ordering::SeqCst), 1, "old one stopped");
        assert_eq!(*f.starts.lock(), vec!["DISPLAY1", "DISPLAY2"]);
        assert_eq!(s.running().as_deref(), Some("DISPLAY2"));
        s.sync(None);
        assert_eq!(f.live.load(Ordering::SeqCst), 0);
        assert_eq!(s.running(), None);
    }

    #[test]
    fn a_failed_start_is_reported_and_cleared() {
        let f = FakeScreens::default();
        let s = ScreenShare::new(Box::new(f.clone()));
        *f.fail.lock() = true;
        s.sync(Some("DISPLAY1"));
        assert_eq!(s.running(), None);
        assert!(s.error().unwrap().contains("monitor gone"));
        *f.fail.lock() = false;
        s.sync(Some("DISPLAY1"));
        assert_eq!(s.error(), None);
        assert_eq!(s.running().as_deref(), Some("DISPLAY1"));
    }
}
