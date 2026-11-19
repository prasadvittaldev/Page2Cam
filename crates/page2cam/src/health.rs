//! Engine health for the tray: a rolling ~10 s view over counters the
//! ticker and webcam slot already keep (`/api/state` → `status.health`).
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use p2c_segment::EngineChoice;

/// Rates are measured over this much recent time.
/// The app takes a [`Sample`] about once a second.
pub const WINDOW: Duration = Duration::from_secs(10);

/// Running totals at one moment.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub at: Instant,
    /// Output ticks run (ticker).
    pub ticks: u64,
    /// Output ticks missed (ticker).
    pub skipped: u64,
    /// Webcam frames delivered (webcam slot `seq`).
    pub webcam_frames: u64,
}

/// The samples of the last [`WINDOW`].
#[derive(Default, Debug)]
pub struct HealthWindow {
    samples: VecDeque<Sample>,
}

impl HealthWindow {
    pub fn push(&mut self, s: Sample) {
        self.samples.push_back(s);
        while self
            .samples
            .front()
            .is_some_and(|f| s.at.saturating_duration_since(f.at) > WINDOW)
        {
            self.samples.pop_front();
        }
    }

    /// Oldest and newest sample, if they span a measurable time.
    fn span(&self) -> Option<(&Sample, &Sample, f64)> {
        let (a, b) = (self.samples.front()?, self.samples.back()?);
        let secs = b.at.saturating_duration_since(a.at).as_secs_f64();
        (secs >= 0.5).then_some((a, b, secs))
    }

    /// Missed ticks in percent of all due ticks (run + missed).
    pub fn skipped_pct(&self) -> f32 {
        let Some((a, b, _)) = self.span() else {
            return 0.0;
        };
        let ran = b.ticks.saturating_sub(a.ticks);
        let missed = if b.skipped >= a.skipped && b.ticks >= a.ticks {
            b.skipped - a.skipped
        } else {
            0
        };
        let due = ran + missed;
        if due == 0 {
            0.0
        } else {
            (missed as f64 * 100.0 / due as f64) as f32
        }
    }

    /// Webcam frames per second.
    pub fn webcam_fps(&self) -> f32 {
        match self.span() {
            Some((a, b, secs)) => {
                (b.webcam_frames.saturating_sub(a.webcam_frames) as f64 / secs) as f32
            }
            None => 0.0,
        }
    }
}

/// "ok" | "reconnecting" | "error" | "off" from the webcam status line
/// (`None`: no webcam in use).
pub fn webcam_state(status: Option<&str>) -> String {
    let s = match status {
        Some(s) if s.starts_with("ok") => "ok",
        Some(s) if s.starts_with("reconnecting") => "reconnecting",
        Some(s) if s.starts_with("error") => "error",
        _ => "off",
    };
    s.into()
}

/// The video status says the GPU path failed and the CPU took over.
pub fn video_fallback(video: &str) -> bool {
    video.starts_with("CPU (fallback")
}

/// The cut-out is in use and runs Fast although Quality was chosen.
pub fn cutout_auto_fallback(
    in_use: bool,
    requested: EngineChoice,
    active: Option<EngineChoice>,
) -> bool {
    in_use && requested == EngineChoice::Quality && active == Some(EngineChoice::Fast)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, s: u64) -> Instant {
        base + Duration::from_secs(s)
    }

    #[test]
    fn health_window_metrics() {
        let base = Instant::now();
        let mut w = HealthWindow::default();
        // Empty / a single sample: nothing to measure yet.
        assert_eq!(w.skipped_pct(), 0.0);
        assert_eq!(w.webcam_fps(), 0.0);
        // Running totals at second `s`: the first 3 s are bad (half the
        // ticks skipped, webcam 5 fps), then 29 ticks + 1 skipped per
        // second and webcam 15 fps.
        let totals = |s: u64| {
            if s <= 3 {
                (15 * s, 15 * s, 5 * s)
            } else {
                (45 + 29 * (s - 3), 45 + (s - 3), 15 + 15 * (s - 3))
            }
        };
        w.push(Sample {
            at: base,
            ticks: 0,
            skipped: 0,
            webcam_frames: 0,
        });
        assert_eq!(w.skipped_pct(), 0.0);
        for s in 1..=15 {
            let (ticks, skipped, webcam_frames) = totals(s);
            w.push(Sample {
                at: at(base, s),
                ticks,
                skipped,
                webcam_frames,
            });
        }
        // Only the last 10 s count: 1 of 30 due ticks skipped, 15 fps.
        let pct = w.skipped_pct();
        assert!((pct - 100.0 / 30.0).abs() < 0.01, "skipped {pct}%");
        let fps = w.webcam_fps();
        assert!((fps - 15.0).abs() < 0.01, "webcam {fps} fps");
    }

    #[test]
    fn counters_going_backwards_read_as_zero() {
        // E.g. a webcam slot replaced: never a negative or huge rate.
        let base = Instant::now();
        let mut w = HealthWindow::default();
        w.push(Sample {
            at: base,
            ticks: 100,
            skipped: 10,
            webcam_frames: 500,
        });
        w.push(Sample {
            at: at(base, 2),
            ticks: 160,
            skipped: 10,
            webcam_frames: 3,
        });
        assert_eq!(w.webcam_fps(), 0.0);
        assert_eq!(w.skipped_pct(), 0.0);
    }

    #[test]
    fn health_flags_from_status_texts() {
        assert_eq!(webcam_state(Some("ok: Integrated Camera")), "ok");
        assert_eq!(
            webcam_state(Some("reconnecting: device lost")),
            "reconnecting"
        );
        assert_eq!(webcam_state(Some("error: no access")), "error");
        assert_eq!(webcam_state(Some("none")), "off");
        assert_eq!(webcam_state(None), "off");
        assert!(video_fallback("CPU (fallback: device removed)"));
        assert!(!video_fallback("CPU (compatibility)"));
        assert!(!video_fallback("GPU: Intel(R) Graphics"));
        use p2c_segment::EngineChoice::{Fast, Quality};
        assert!(cutout_auto_fallback(true, Quality, Some(Fast)));
        assert!(!cutout_auto_fallback(false, Quality, Some(Fast)));
        assert!(!cutout_auto_fallback(true, Fast, Some(Fast)));
        assert!(!cutout_auto_fallback(true, Quality, Some(Quality)));
        assert!(!cutout_auto_fallback(true, Quality, None));
    }
}
