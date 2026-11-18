//! Small timing helpers for the periodic performance lines in the log.
use std::fmt;
use std::time::{Duration, Instant};

/// At most this many samples are kept per window (newer ones are dropped).
pub const MAX_SAMPLES: usize = 4096;

/// Collects durations over a window and summarizes them.
#[derive(Debug, Default)]
pub struct DurationStats {
    samples: Vec<Duration>,
}

/// Median, 95th percentile and maximum of a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub count: usize,
    pub p50: Duration,
    pub p95: Duration,
    pub max: Duration,
}

impl DurationStats {
    pub fn record(&mut self, d: Duration) {
        if self.samples.len() < MAX_SAMPLES {
            self.samples.push(d);
        }
    }

    pub fn count(&self) -> usize {
        self.samples.len()
    }

    pub fn summary(&self) -> Option<Summary> {
        if self.samples.is_empty() {
            return None;
        }
        let mut s = self.samples.clone();
        s.sort_unstable();
        // Nearest-rank percentile.
        let at = |p: usize| s[((s.len() * p).div_ceil(100)).clamp(1, s.len()) - 1];
        Some(Summary {
            count: s.len(),
            p50: at(50),
            p95: at(95),
            max: s[s.len() - 1],
        })
    }

    /// Summary of the window so far, then start a new window.
    pub fn take_summary(&mut self) -> Option<Summary> {
        let s = self.summary();
        self.samples.clear();
        s
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        write!(
            f,
            "p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms (n={})",
            ms(self.p50),
            ms(self.p95),
            ms(self.max),
            self.count
        )
    }
}

/// Counts events and reports their rate once per `window`.
#[derive(Debug)]
pub struct RateWindow {
    window: Duration,
    start: Instant,
    count: u32,
}

impl RateWindow {
    pub fn new(window: Duration, now: Instant) -> RateWindow {
        RateWindow {
            window,
            start: now,
            count: 0,
        }
    }

    /// Record one event. Returns events per second when a window has ended.
    pub fn tick(&mut self, now: Instant) -> Option<f64> {
        self.count += 1;
        let el = now.saturating_duration_since(self.start);
        if el < self.window {
            return None;
        }
        let rate = self.count as f64 / el.as_secs_f64();
        self.start = now;
        self.count = 0;
        Some(rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn empty_stats_report_nothing() {
        let s = DurationStats::default();
        assert_eq!(s.count(), 0);
        assert_eq!(s.summary(), None);
    }

    #[test]
    fn summary_has_percentiles_and_max() {
        let mut s = DurationStats::default();
        for n in 1..=100 {
            s.record(ms(n));
        }
        let sum = s.summary().unwrap();
        assert_eq!(sum.count, 100);
        assert_eq!(sum.p50, ms(50));
        assert_eq!(sum.p95, ms(95));
        assert_eq!(sum.max, ms(100));
    }

    #[test]
    fn take_summary_starts_a_new_window() {
        let mut s = DurationStats::default();
        s.record(ms(7));
        assert_eq!(s.take_summary().unwrap().max, ms(7));
        assert_eq!(s.count(), 0);
        s.record(ms(3));
        assert_eq!(s.take_summary().unwrap().max, ms(3));
    }

    #[test]
    fn display_is_compact_ms() {
        let mut s = DurationStats::default();
        s.record(Duration::from_micros(1500));
        assert_eq!(
            s.summary().unwrap().to_string(),
            "p50 1.5 ms, p95 1.5 ms, max 1.5 ms (n=1)"
        );
    }

    #[test]
    fn rate_window_reports_once_per_window() {
        let t0 = std::time::Instant::now();
        let mut r = RateWindow::new(Duration::from_secs(10), t0);
        for i in 1..=150 {
            assert_eq!(r.tick(t0 + Duration::from_millis(i * 60)), None);
        }
        // 151st event lands after the 10 s window: 151 events / 10.02 s.
        let fps = r.tick(t0 + Duration::from_millis(10_020)).unwrap();
        assert!((fps - 15.07).abs() < 0.05, "{fps}");
        assert_eq!(
            r.tick(t0 + Duration::from_millis(10_050)),
            None,
            "window restarted"
        );
    }

    #[test]
    fn window_is_bounded() {
        let mut s = DurationStats::default();
        for _ in 0..(MAX_SAMPLES + 50) {
            s.record(ms(1));
        }
        assert_eq!(s.count(), MAX_SAMPLES);
    }
}
