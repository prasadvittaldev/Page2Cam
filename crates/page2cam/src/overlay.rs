//! Overlay layers in the engine: the layer list (saved in
//! `<data dir>/overlays/layers.json`, files in `overlays/assets/`), and the
//! overlay browser's on/off. While no layer is visible the overlay browser's
//! paints are dropped, it paints at 1 fps and the compositors skip the
//! overlay pass; showing a layer turns it back on at the page's frame rate.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use p2c_browser::BrowserHandle;
use p2c_compositor::OverlayFeed;
use p2c_overlay::{Assets, LayerSet};
use parking_lot::Mutex;
use serde_json::Value;

/// After the last layer is hidden, keep the overlay running this long (its
/// exit animation, then one transparent paint) before pausing it.
pub const IDLE_AFTER: Duration = Duration::from_millis(1200);

/// Whether the overlay should be on now (`Some`) when that differs from
/// `active` (`None`: not decided yet). `hidden_since`: since when nothing
/// has been visible while still on.
pub fn decide(
    any_visible: bool,
    active: Option<bool>,
    hidden_since: &mut Option<Instant>,
    now: Instant,
) -> Option<bool> {
    if any_visible {
        *hidden_since = None;
        return (active != Some(true)).then_some(true);
    }
    match active {
        None => Some(false),
        Some(false) => None,
        Some(true) => {
            let since = *hidden_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= IDLE_AFTER {
                *hidden_since = None;
                Some(false)
            } else {
                None
            }
        }
    }
}

#[derive(Default)]
struct Gov {
    active: Option<bool>,
    hidden_since: Option<Instant>,
}

/// The engine's overlay layers.
pub struct OverlayLayers {
    set: Mutex<LayerSet>,
    assets: Assets,
    file: Option<PathBuf>,
    /// For the ticker: CPU frames and "any layer visible".
    pub feed: OverlayFeed,
    /// The overlay browser's paints are dropped while set (shared with
    /// `OverlayBrowser.paused`); set at start (nothing visible yet).
    pub paused: Arc<AtomicBool>,
    browser: OnceLock<BrowserHandle>,
    gov: Mutex<Gov>,
    /// The page frame rate (the throttle's frame-rate lever; the overlay
    /// follows it, never the resolution).
    fps: AtomicU32,
}

impl OverlayLayers {
    /// Load the saved layers (all hidden) from `data_dir/overlays/`;
    /// `None`: kept in memory only.
    pub fn new(data_dir: Option<&Path>, fps: u32) -> OverlayLayers {
        let dir = data_dir.map(|d| d.join("overlays"));
        let assets = Assets::new(
            dir.as_ref()
                .map(|d| d.join("assets"))
                .unwrap_or_else(|| std::env::temp_dir().join("page2cam-overlay-assets")),
        );
        let file = dir.as_ref().map(|d| d.join("layers.json"));
        let set = match &file {
            Some(f) => match LayerSet::load(f) {
                Ok((s, bad)) => {
                    for e in bad {
                        tracing::warn!("overlay layer dropped: {e}");
                    }
                    if !s.layers().is_empty() {
                        tracing::info!(layers = s.layers().len(), "overlay layers loaded (hidden)");
                    }
                    s
                }
                Err(e) => {
                    tracing::warn!("overlay layers not loaded: {e:#}");
                    LayerSet::new()
                }
            },
            None => LayerSet::new(),
        };
        if dir.is_some() {
            assets.prune(&set.assets_in_use());
        }
        OverlayLayers {
            set: Mutex::new(set),
            assets,
            file,
            feed: OverlayFeed::default(),
            paused: Arc::new(AtomicBool::new(true)),
            browser: OnceLock::new(),
            gov: Mutex::default(),
            fps: AtomicU32::new(fps.max(1)),
        }
    }

    /// The overlay browser exists: apply the current on/off to it.
    pub fn set_browser(&self, b: BrowserHandle) {
        let _ = self.browser.set(b);
        let on = self.gov.lock().active;
        if let Some(on) = on {
            self.apply(on);
        }
        self.govern(Instant::now());
    }

    /// The page frame rate changed (the overlay follows it while on).
    pub fn set_fps(&self, fps: u32) {
        self.fps.store(fps.max(1), Ordering::Relaxed);
        if self.gov.lock().active == Some(true) {
            if let Some(b) = self.browser.get() {
                b.set_frame_rate(fps.max(1));
            }
        }
    }

    fn apply(&self, on: bool) {
        self.feed.active.store(on, Ordering::Relaxed);
        if let Some(b) = self.browser.get() {
            if on {
                b.set_frame_rate(self.fps.load(Ordering::Relaxed));
                b.set_paused(false);
            } else {
                b.set_paused(true);
                b.set_frame_rate(1);
            }
        } else {
            self.paused.store(!on, Ordering::SeqCst);
        }
    }

    /// Turn the overlay on or off for what is visible now.
    fn govern(&self, now: Instant) {
        let any = self.set.lock().any_visible();
        let mut g = self.gov.lock();
        let Gov {
            active,
            hidden_since,
        } = &mut *g;
        if let Some(on) = decide(any, *active, hidden_since, now) {
            *active = Some(on);
            drop(g);
            tracing::info!(on, "overlay layers {}", if on { "shown" } else { "idle" });
            self.apply(on);
        }
    }

    /// Every ~250 ms: auto-hide due layers, then on/off.
    pub fn tick(&self, now: Instant) {
        let hid = self.set.lock().tick(now);
        if hid {
            self.save();
        }
        self.govern(now);
    }

    fn save(&self) {
        let Some(f) = &self.file else {
            return;
        };
        if let Err(e) = self.set.lock().save(f) {
            tracing::warn!("overlay layers not saved: {e:#}");
        }
    }

    /// Run a change, then save and turn the overlay on/off.
    fn change<T>(&self, f: impl FnOnce(&mut LayerSet, &Assets) -> Result<T>) -> Result<T> {
        let r = f(&mut self.set.lock(), &self.assets)?;
        self.save();
        self.govern(Instant::now());
        Ok(r)
    }

    pub fn layers(&self) -> Vec<Value> {
        self.set
            .lock()
            .layers()
            .iter()
            .map(|l| l.to_json())
            .collect()
    }

    pub fn add(&self, spec: Value) -> Result<Value> {
        self.change(|s, a| Ok(s.add(spec, a, Instant::now())?.to_json()))
    }

    pub fn update(&self, id: &str, patch: Value) -> Result<Value> {
        self.change(|s, a| Ok(s.update(id, patch, a, Instant::now())?.to_json()))
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        self.change(|s, a| s.remove(id, a).map(|_| ()))
    }

    pub fn clear(&self) -> usize {
        self.change(|s, _| Ok(s.clear())).unwrap_or(0)
    }

    pub fn reorder(&self, ids: Vec<String>) -> Result<()> {
        self.change(|s, _| s.reorder(&ids))
    }

    pub fn set_asset(&self, id: &str, bytes: &[u8]) -> Result<Value> {
        self.change(|s, a| Ok(s.set_asset(id, bytes, a)?.to_json()))
    }

    pub fn state(&self, since: Option<u64>) -> Option<Value> {
        let s = self.set.lock();
        (since != Some(s.rev())).then(|| s.state_json())
    }

    pub fn html_of(&self, id: &str) -> Option<String> {
        self.set.lock().html_of(id).map(str::to_string)
    }

    pub fn asset(&self, name: &str) -> Option<(Vec<u8>, &'static str)> {
        self.assets.read(name)
    }

    /// How many layers are visible (status line).
    pub fn visible_count(&self) -> usize {
        self.set
            .lock()
            .layers()
            .iter()
            .filter(|l| l.visible)
            .count()
    }

    /// Whether the overlay pass is on.
    pub fn is_active(&self) -> bool {
        self.feed.active.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn idle_after_the_exit_animation_on_at_once() {
        let t = Instant::now();
        let mut hs = None;
        // Start: nothing visible → off at once.
        assert_eq!(decide(false, None, &mut hs, t), Some(false));
        assert_eq!(decide(false, Some(false), &mut hs, t), None);
        // A layer shows → on at once.
        assert_eq!(decide(true, Some(false), &mut hs, t), Some(true));
        assert_eq!(decide(true, Some(true), &mut hs, t), None);
        // Hidden: stays on for the exit animation, then off.
        assert_eq!(decide(false, Some(true), &mut hs, t), None);
        let soon = t + Duration::from_millis(600);
        assert_eq!(decide(false, Some(true), &mut hs, soon), None);
        // Shown again meanwhile: the idle wait starts over.
        assert_eq!(decide(true, Some(true), &mut hs, soon), None);
        assert_eq!(decide(false, Some(true), &mut hs, soon), None);
        assert_eq!(
            decide(
                false,
                Some(true),
                &mut hs,
                soon + Duration::from_millis(1100)
            ),
            None
        );
        assert_eq!(
            decide(false, Some(true), &mut hs, soon + IDLE_AFTER),
            Some(false)
        );
    }

    #[test]
    fn layers_drive_the_feed_and_are_saved_hidden() {
        let d = tempfile::tempdir().unwrap();
        let o = OverlayLayers::new(Some(d.path()), 30);
        o.tick(Instant::now());
        assert!(!o.is_active());
        assert!(
            o.paused.load(Ordering::SeqCst),
            "paused while nothing shows"
        );
        let l = o
            .add(json!({"kind": "title", "text": "Live", "id": "t", "hide_after_s": 1}))
            .unwrap();
        assert_eq!(l["id"], "t");
        assert!(o.is_active());
        assert!(!o.paused.load(Ordering::SeqCst));
        assert_eq!(o.visible_count(), 1);
        assert!(d.path().join("overlays/layers.json").exists());
        let rev = o.state(None).unwrap()["rev"].as_u64().unwrap();
        assert!(o.state(Some(rev)).is_none());
        // Auto-hide, then idle after the exit animation.
        let t = Instant::now() + Duration::from_secs(2);
        o.tick(t);
        assert_eq!(o.visible_count(), 0);
        assert!(o.is_active());
        o.tick(t + IDLE_AFTER);
        assert!(!o.is_active());
        // A restart brings the layer back hidden.
        o.update("t", json!({"visible": true, "hide_after_s": null}))
            .unwrap();
        let again = OverlayLayers::new(Some(d.path()), 30);
        assert_eq!(again.layers().len(), 1);
        assert_eq!(again.visible_count(), 0);
    }
}
