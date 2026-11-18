//! Which compositor makes the video (GPU or CPU), its status text, and the
//! watchdog that falls back to the CPU when Chromium's GPU frames don't come.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Status text: the GPU path on this adapter.
pub fn status_gpu(adapter: &str) -> String {
    format!("GPU: {adapter}")
}

/// Status text: Chromium ignored the adapter switch and renders on
/// `adapter`, so the video moved there.
pub fn status_gpu_chromium_chose(adapter: &str) -> String {
    format!("GPU: {adapter} (Chromium chose it)")
}

/// Status text: switched to the CPU at run time (or before CEF started).
pub fn status_fallback(reason: &str) -> String {
    format!("CPU (fallback: {reason})")
}

/// Status text: the CPU path by choice (`gpu = "cpu"`, no hardware GPU),
/// i.e. the 0.3.0 behaviour.
pub const STATUS_COMPAT: &str = "CPU (compatibility)";

/// Status text: `--test-pattern` (it paints into the CPU page slot).
pub const STATUS_TEST_PATTERN: &str = "CPU (test pattern)";

/// How long Chromium may take to deliver its first GPU frame (a dGPU can
/// need 1-3 s to wake up and the page to load).
pub const FIRST_TEXTURE_WITHIN: Duration = Duration::from_secs(5);
/// Failed shared-texture opens in a row that mean the GPU path is broken.
pub const MAX_FAILED_OPENS: u32 = 10;

/// What the browser's shared-texture callback has seen.
#[derive(Default)]
pub struct TextureStats {
    ok: AtomicU64,
    failed_in_row: AtomicU32,
    last_error: Mutex<String>,
    last_warn: Mutex<Option<Instant>>,
}

impl TextureStats {
    pub fn record_ok(&self) {
        self.ok.fetch_add(1, Ordering::Relaxed);
        self.failed_in_row.store(0, Ordering::Relaxed);
    }

    /// A failed open; logged at most once a second.
    pub fn record_err(&self, error: String) {
        self.failed_in_row.fetch_add(1, Ordering::Relaxed);
        let mut w = self.last_warn.lock();
        if w.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            tracing::warn!(%error, "page texture from Chromium could not be opened");
            *w = Some(Instant::now());
        }
        *self.last_error.lock() = error;
    }

    /// Why the GPU path should be given up, if it should, `elapsed` after
    /// the browser was created.
    pub fn verdict(&self, elapsed: Duration) -> Option<String> {
        watchdog_verdict(
            elapsed,
            self.ok.load(Ordering::Relaxed),
            self.failed_in_row.load(Ordering::Relaxed),
            &self.last_error.lock(),
        )
    }
}

/// No texture within [`FIRST_TEXTURE_WITHIN`], or [`MAX_FAILED_OPENS`]
/// failed opens in a row → the reason to fall back to the CPU.
pub fn watchdog_verdict(
    elapsed: Duration,
    ok: u64,
    failed_in_row: u32,
    last_error: &str,
) -> Option<String> {
    if failed_in_row >= MAX_FAILED_OPENS {
        return Some(format!(
            "Chromium's page texture could not be opened {failed_in_row} times in a row: {last_error}"
        ));
    }
    if ok == 0 && elapsed >= FIRST_TEXTURE_WITHIN {
        return Some(format!(
            "no page frame from Chromium on the GPU within {} s",
            FIRST_TEXTURE_WITHIN.as_secs()
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_texts() {
        assert_eq!(
            status_gpu("NVIDIA RTX 2000 Ada"),
            "GPU: NVIDIA RTX 2000 Ada"
        );
        assert_eq!(
            status_fallback("device lost"),
            "CPU (fallback: device lost)"
        );
        assert_eq!(STATUS_COMPAT, "CPU (compatibility)");
        assert_eq!(
            status_gpu_chromium_chose("Intel(R) UHD Graphics"),
            "GPU: Intel(R) UHD Graphics (Chromium chose it)"
        );
    }

    #[test]
    fn watchdog_tolerates_a_slow_first_frame() {
        let s = Duration::from_secs;
        assert_eq!(watchdog_verdict(s(3), 0, 0, ""), None);
        assert_eq!(
            watchdog_verdict(Duration::from_millis(4900), 0, 3, "x"),
            None
        );
        let r = watchdog_verdict(s(5), 0, 0, "").expect("fallback after 5 s");
        assert!(r.contains("within 5 s"), "{r}");
        // Frames arrived: no time limit afterwards.
        assert_eq!(watchdog_verdict(s(60), 1, 0, ""), None);
    }

    #[test]
    fn watchdog_gives_up_after_ten_failed_opens_in_a_row() {
        let s = Duration::from_secs;
        assert_eq!(watchdog_verdict(s(1), 100, 9, "E_INVALIDARG"), None);
        let r = watchdog_verdict(s(1), 100, 10, "E_INVALIDARG").unwrap();
        assert!(r.contains("10 times") && r.contains("E_INVALIDARG"), "{r}");
    }

    #[test]
    fn stats_count_failures_in_a_row() {
        let st = TextureStats::default();
        for _ in 0..9 {
            st.record_err("bad handle".into());
        }
        st.record_ok();
        for _ in 0..9 {
            st.record_err("bad handle".into());
        }
        assert_eq!(st.verdict(Duration::from_secs(1)), None);
        st.record_err("bad handle".into());
        assert!(st.verdict(Duration::from_secs(1)).is_some());
    }
}
