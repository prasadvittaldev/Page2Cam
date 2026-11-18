//! When Quality is too slow, switch to Fast.
use std::time::{Duration, Instant};

/// Inference slower than this is too slow even for a fast camera (15 masks/s).
const MIN_INTERVAL: Duration = Duration::from_micros(66_667);
/// How long inference must stay too slow before falling back.
const WINDOW: Duration = Duration::from_secs(3);

/// Decides when the Quality engine can't keep up. Judged on inference time
/// against the camera's frame interval, so a slow camera never triggers it.
#[derive(Debug, Default)]
pub struct FallbackPolicy {
    slow_since: Option<Instant>,
}

impl FallbackPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one inference. True once every inference for `WINDOW` took
    /// longer than `max(cam_interval, 1/15 s)`.
    pub fn observe(&mut self, infer: Duration, cam_interval: Duration, now: Instant) -> bool {
        if infer <= cam_interval.max(MIN_INTERVAL) {
            self.slow_since = None;
            return false;
        }
        let since = *self.slow_since.get_or_insert(now);
        now.saturating_duration_since(since) >= WINDOW
    }

    /// Start a fresh window (after a manual engine switch).
    pub fn rearm(&mut self) {
        self.slow_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAM_30: Duration = Duration::from_micros(33_333);

    #[test]
    fn policy_falls_back_after_3s_slow() {
        let mut p = FallbackPolicy::new();
        let t0 = Instant::now();
        for k in 0..30 {
            let now = t0 + Duration::from_millis(100 * k);
            assert!(
                !p.observe(Duration::from_millis(100), CAM_30, now),
                "fired at {k}"
            );
        }
        assert!(p.observe(
            Duration::from_millis(100),
            CAM_30,
            t0 + Duration::from_secs(3)
        ));
    }

    #[test]
    fn policy_ignores_slow_camera() {
        let mut p = FallbackPolicy::new();
        let t0 = Instant::now();
        for k in 0..60 {
            let now = t0 + Duration::from_millis(100 * k);
            assert!(!p.observe(Duration::from_millis(80), Duration::from_secs(1), now));
        }
    }

    #[test]
    fn policy_fast_frame_resets_window() {
        let mut p = FallbackPolicy::new();
        let t0 = Instant::now();
        let slow = Duration::from_millis(100);
        p.observe(slow, CAM_30, t0);
        p.observe(
            Duration::from_millis(10),
            CAM_30,
            t0 + Duration::from_secs(2),
        );
        assert!(!p.observe(slow, CAM_30, t0 + Duration::from_millis(3500)));
    }

    #[test]
    fn policy_rearm_restarts_window() {
        let mut p = FallbackPolicy::new();
        let t0 = Instant::now();
        let slow = Duration::from_millis(100);
        p.observe(slow, CAM_30, t0);
        p.rearm();
        assert!(!p.observe(slow, CAM_30, t0 + Duration::from_millis(2900)));
        assert!(!p.observe(slow, CAM_30, t0 + Duration::from_millis(5000)));
        assert!(p.observe(slow, CAM_30, t0 + Duration::from_millis(5900)));
    }
}
