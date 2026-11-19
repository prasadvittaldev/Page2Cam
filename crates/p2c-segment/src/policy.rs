//! When Quality is too slow, switch to Fast, and try Quality again later.
use std::time::{Duration, Instant};

/// Inference slower than this is too slow even for a fast camera (15 masks/s).
const MIN_INTERVAL: Duration = Duration::from_micros(66_667);

/// The policy's time constants (R42); tests scale them down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PolicyTimes {
    /// How long inference must stay too slow before falling back.
    pub window: Duration,
    /// Wait before retrying Quality after the first automatic fallback;
    /// doubled after each further one.
    pub first_retry: Duration,
    /// Longest wait between retries. Quality keeping up this long after a
    /// retry also starts the backoff over.
    pub max_retry: Duration,
}

impl Default for PolicyTimes {
    fn default() -> Self {
        PolicyTimes {
            window: Duration::from_secs(10),
            first_retry: Duration::from_secs(60),
            max_retry: Duration::from_secs(600),
        }
    }
}

/// Decides when the Quality engine can't keep up, and when to try it again.
/// Judged on inference time against the camera's frame interval, so a slow
/// camera never triggers it. A busy GPU (e.g. a heavy page loading) can make
/// Quality slow for a while, so a fallback is not final: Quality is retried
/// after 60 s, then 120 s, 240 s ... up to 10 min.
#[derive(Debug)]
pub struct FallbackPolicy {
    times: PolicyTimes,
    slow_since: Option<Instant>,
    /// On the automatic Fast: when to retry Quality.
    retry_at: Option<Instant>,
    /// The wait after the next automatic fallback.
    next_wait: Duration,
    /// When Quality was last retried (to reset the backoff once it keeps up).
    retried_at: Option<Instant>,
}

impl Default for FallbackPolicy {
    fn default() -> Self {
        Self::with_times(PolicyTimes::default())
    }
}

impl FallbackPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_times(times: PolicyTimes) -> Self {
        FallbackPolicy {
            times,
            slow_since: None,
            retry_at: None,
            next_wait: times.first_retry,
            retried_at: None,
        }
    }

    /// Record one Quality inference. True once every inference for the
    /// window (10 s) took longer than `max(cam_interval, 1/15 s)`.
    pub fn observe(&mut self, infer: Duration, cam_interval: Duration, now: Instant) -> bool {
        if infer <= cam_interval.max(MIN_INTERVAL) {
            self.slow_since = None;
            if self
                .retried_at
                .is_some_and(|t| now.saturating_duration_since(t) >= self.times.max_retry)
            {
                // Quality kept up after the retry: the next fallback waits
                // the shortest time again.
                self.retried_at = None;
                self.next_wait = self.times.first_retry;
            }
            return false;
        }
        let since = *self.slow_since.get_or_insert(now);
        now.saturating_duration_since(since) >= self.times.window
    }

    /// An automatic fallback to Fast happened at `now`: schedule the retry.
    /// Returns the wait; the one after the next fallback is twice as long
    /// (capped).
    pub fn fell_back(&mut self, now: Instant) -> Duration {
        let wait = self.next_wait;
        self.retry_at = Some(now + wait);
        self.next_wait = (wait * 2).min(self.times.max_retry);
        self.slow_since = None;
        self.retried_at = None;
        wait
    }

    /// Time left until Quality is retried (`None` unless on the automatic Fast).
    pub fn retry_in(&self, now: Instant) -> Option<Duration> {
        self.retry_at.map(|t| t.saturating_duration_since(now))
    }

    /// Whether it's time to try Quality again.
    pub fn retry_due(&self, now: Instant) -> bool {
        self.retry_at.is_some_and(|t| now >= t)
    }

    /// Quality is being retried now: a fresh slowness window.
    pub fn retrying(&mut self, now: Instant) {
        self.retry_at = None;
        self.slow_since = None;
        self.retried_at = Some(now);
    }

    /// Start over (after a manual engine choice): fresh window, no pending
    /// retry, shortest wait.
    pub fn rearm(&mut self) {
        *self = Self::with_times(self.times);
    }
}

/// The admin page note while on the automatic Fast (seconds rounded up).
pub fn too_slow_note(retry_in: Duration) -> String {
    let secs = retry_in.as_millis().div_ceil(1000).max(1);
    format!("Quality was too slow (busy GPU) \u{2014} trying Quality again in {secs} s")
}
#[cfg(test)]
mod tests {
    use super::*;

    const CAM_30: Duration = Duration::from_micros(33_333);
    const SLOW: Duration = Duration::from_millis(100);

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// Feed slow inferences every 100 ms from `from` to `to` (exclusive);
    /// the time of the first one that triggers a fallback.
    fn slow_until(p: &mut FallbackPolicy, from: Instant, to: Instant) -> Option<Instant> {
        let mut now = from;
        while now < to {
            if p.observe(SLOW, CAM_30, now) {
                return Some(now);
            }
            now += Duration::from_millis(100);
        }
        None
    }

    #[test]
    fn policy_no_fallback_for_9s_of_slowness() {
        let mut p = FallbackPolicy::new();
        let t0 = Instant::now();
        assert_eq!(
            slow_until(&mut p, t0, t0 + s(9) + Duration::from_millis(900)),
            None
        );
    }

    #[test]
    fn policy_falls_back_after_10s_slow() {
        let mut p = FallbackPolicy::new();
        let t0 = Instant::now();
        assert_eq!(slow_until(&mut p, t0, t0 + s(11)), Some(t0 + s(10)));
    }

    #[test]
    fn policy_ignores_slow_camera() {
        let mut p = FallbackPolicy::new();
        let t0 = Instant::now();
        for k in 0..200 {
            let now = t0 + Duration::from_millis(100 * k);
            assert!(!p.observe(Duration::from_millis(80), Duration::from_secs(1), now));
        }
    }

    #[test]
    fn policy_fast_frame_resets_window() {
        let mut p = FallbackPolicy::new();
        let t0 = Instant::now();
        p.observe(SLOW, CAM_30, t0);
        p.observe(Duration::from_millis(10), CAM_30, t0 + s(8));
        assert!(!p.observe(SLOW, CAM_30, t0 + s(11)));
        assert!(!p.observe(SLOW, CAM_30, t0 + s(20)));
        assert!(p.observe(SLOW, CAM_30, t0 + s(21)));
    }

    #[test]
    fn policy_retries_quality_after_60s() {
        let mut p = FallbackPolicy::new();
        let t0 = Instant::now();
        assert_eq!(p.retry_in(t0), None, "no retry before any fallback");
        assert_eq!(p.fell_back(t0), s(60));
        assert_eq!(p.retry_in(t0 + s(15)), Some(s(45)));
        assert!(!p.retry_due(t0 + s(59)));
        assert!(p.retry_due(t0 + s(60)));
        p.retrying(t0 + s(60));
        assert!(!p.retry_due(t0 + s(61)));
        assert_eq!(p.retry_in(t0 + s(61)), None);
    }

    #[test]
    fn policy_retry_starts_a_fresh_window() {
        let mut p = FallbackPolicy::new();
        let t0 = Instant::now();
        assert!(slow_until(&mut p, t0, t0 + s(11)).is_some());
        p.fell_back(t0 + s(10));
        p.retrying(t0 + s(70));
        // Slow again right away: still 10 s before the next fallback.
        assert_eq!(slow_until(&mut p, t0 + s(70), t0 + s(90)), Some(t0 + s(80)));
    }

    #[test]
    fn policy_backoff_doubles_up_to_10_min() {
        let mut p = FallbackPolicy::new();
        let mut now = Instant::now();
        let mut waits = Vec::new();
        for _ in 0..7 {
            let w = p.fell_back(now);
            waits.push(w.as_secs());
            now += w;
            assert!(p.retry_due(now));
            p.retrying(now);
            now += s(10);
        }
        assert_eq!(waits, [60, 120, 240, 480, 600, 600, 600]);
    }

    #[test]
    fn policy_backoff_resets_after_10_min_of_good_quality() {
        let mut p = FallbackPolicy::new();
        let t0 = Instant::now();
        assert_eq!(p.fell_back(t0), s(60));
        p.retrying(t0 + s(60));
        assert_eq!(p.fell_back(t0 + s(100)), s(120));
        p.retrying(t0 + s(220));
        // Quality keeps up for 10 minutes after the retry.
        let fine = Duration::from_millis(20);
        assert!(!p.observe(fine, CAM_30, t0 + s(500)));
        assert!(!p.observe(fine, CAM_30, t0 + s(820)));
        assert_eq!(p.fell_back(t0 + s(900)), s(60));
    }

    #[test]
    fn policy_rearm_restarts_window_and_backoff() {
        let mut p = FallbackPolicy::new();
        let t0 = Instant::now();
        p.observe(SLOW, CAM_30, t0);
        p.rearm();
        assert_eq!(slow_until(&mut p, t0 + s(5), t0 + s(20)), Some(t0 + s(15)));
        assert_eq!(p.fell_back(t0 + s(15)), s(60));
        p.retrying(t0 + s(75));
        assert_eq!(p.fell_back(t0 + s(90)), s(120));
        // A manual engine choice: no pending retry, the backoff starts over.
        p.rearm();
        assert_eq!(p.retry_in(t0 + s(100)), None);
        assert!(!p.retry_due(t0 + s(1000)));
        assert_eq!(p.fell_back(t0 + s(1000)), s(60));
    }

    #[test]
    fn policy_short_times_for_tests() {
        let t = PolicyTimes {
            window: Duration::from_millis(300),
            first_retry: Duration::from_millis(500),
            max_retry: Duration::from_millis(1500),
        };
        let mut p = FallbackPolicy::with_times(t);
        let t0 = Instant::now();
        assert_eq!(
            slow_until(&mut p, t0, t0 + s(1)),
            Some(t0 + Duration::from_millis(300))
        );
        assert_eq!(p.fell_back(t0), Duration::from_millis(500));
    }

    #[test]
    fn too_slow_note_text() {
        assert_eq!(
            too_slow_note(s(60)),
            "Quality was too slow (busy GPU) \u{2014} trying Quality again in 60 s"
        );
        // Rounded up, never "0 s" while waiting.
        assert_eq!(
            too_slow_note(Duration::from_millis(200)),
            "Quality was too slow (busy GPU) \u{2014} trying Quality again in 1 s"
        );
        assert_eq!(
            too_slow_note(Duration::from_millis(59_001)),
            "Quality was too slow (busy GPU) \u{2014} trying Quality again in 60 s"
        );
    }
}
