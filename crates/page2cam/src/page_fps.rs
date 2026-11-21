//! Slows the page down while the output can't keep up.
//!
//! A heavy page (e.g. a WebGL animation) can take the whole GPU, and then
//! Chromium's work starves our compositor: the output skips most ticks. The
//! page's frame rate is the one knob that frees the GPU without touching the
//! webcam or the output rate, so [`PageFps`] steps it down while the output
//! skips ticks and back up once it has kept up for a while.

use std::time::{Duration, Instant};

/// Output ticks skipped (percent, over the health window) that count as
/// "can't keep up".
pub const SLOW_PCT: f32 = 10.0;
/// At most this many percent skipped counts as "keeping up".
pub const OK_PCT: f32 = 2.0;
/// Slow for this long → one step down.
pub const STEP_DOWN_AFTER: Duration = Duration::from_secs(3);
/// After a change, wait this long before the next step down: the skipped
/// percentage is a rolling ~10 s figure and still shows the old rate.
pub const SETTLE: Duration = Duration::from_secs(10);
/// Keeping up for this long → one step up.
pub const STEP_UP_AFTER: Duration = Duration::from_secs(30);
/// Page frame rates below the configured one, fastest first.
const STEPS: [u32; 4] = [30, 20, 15, 10];

/// Chooses the page frame rate from the output's skipped ticks.
#[derive(Debug)]
pub struct PageFps {
    levels: Vec<u32>,
    at: usize,
    slow_since: Option<Instant>,
    ok_since: Option<Instant>,
    changed_at: Option<Instant>,
}

impl PageFps {
    /// `max`: the configured frame rate (the page's normal rate).
    pub fn new(max: u32) -> Self {
        let mut levels = vec![max.max(1)];
        levels.extend(STEPS.iter().copied().filter(|&s| s < max));
        Self {
            levels,
            at: 0,
            slow_since: None,
            ok_since: None,
            changed_at: None,
        }
    }

    /// The page frame rate now.
    pub fn fps(&self) -> u32 {
        self.levels[self.at]
    }

    /// Feed one reading (about once a second). Returns the new page frame
    /// rate when it changes.
    pub fn update(&mut self, now: Instant, skipped_pct: f32) -> Option<u32> {
        let settled = self
            .changed_at
            .is_none_or(|t| now.saturating_duration_since(t) >= SETTLE);
        if skipped_pct > SLOW_PCT {
            self.ok_since = None;
            let since = *self.slow_since.get_or_insert(now);
            if settled
                && now.saturating_duration_since(since) >= STEP_DOWN_AFTER
                && self.at + 1 < self.levels.len()
            {
                return Some(self.step(now, self.at + 1));
            }
        } else {
            self.slow_since = None;
            if skipped_pct <= OK_PCT {
                let since = *self.ok_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= STEP_UP_AFTER && self.at > 0 {
                    return Some(self.step(now, self.at - 1));
                }
            } else {
                self.ok_since = None;
            }
        }
        None
    }

    fn step(&mut self, now: Instant, to: usize) -> u32 {
        self.at = to;
        self.changed_at = Some(now);
        self.slow_since = None;
        self.ok_since = None;
        self.fps()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `pct` once a second for `secs` seconds from `*t`; the changes.
    fn run(g: &mut PageFps, t: &mut Instant, secs: u64, pct: f32) -> Vec<u32> {
        let mut out = vec![];
        for _ in 0..secs {
            if let Some(f) = g.update(*t, pct) {
                out.push(f);
            }
            *t += Duration::from_secs(1);
        }
        out
    }

    #[test]
    fn keeps_the_configured_rate_while_the_output_keeps_up() {
        let (mut g, mut t) = (PageFps::new(30), Instant::now());
        assert_eq!(run(&mut g, &mut t, 120, 0.0), Vec::<u32>::new());
        assert_eq!(run(&mut g, &mut t, 120, 5.0), Vec::<u32>::new());
        assert_eq!(g.fps(), 30);
    }

    #[test]
    fn steps_down_while_skipping_waiting_for_the_window_to_settle() {
        let (mut g, mut t) = (PageFps::new(30), Instant::now());
        // 3 s slow → 20; then one step per 10 s settle, down to 10.
        assert_eq!(run(&mut g, &mut t, 4, 90.0), vec![20]);
        assert_eq!(run(&mut g, &mut t, 9, 90.0), Vec::<u32>::new());
        assert_eq!(run(&mut g, &mut t, 1, 90.0), vec![15]);
        assert_eq!(run(&mut g, &mut t, 60, 90.0), vec![10]);
        assert_eq!(g.fps(), 10, "never below the last step");
    }

    #[test]
    fn a_short_spike_changes_nothing() {
        let (mut g, mut t) = (PageFps::new(30), Instant::now());
        for _ in 0..10 {
            run(&mut g, &mut t, 2, 50.0);
            run(&mut g, &mut t, 1, 0.0);
        }
        assert_eq!(g.fps(), 30);
    }

    #[test]
    fn steps_back_up_after_keeping_up_for_30_s() {
        let (mut g, mut t) = (PageFps::new(30), Instant::now());
        run(&mut g, &mut t, 14, 90.0);
        assert_eq!(g.fps(), 15);
        assert_eq!(run(&mut g, &mut t, 30, 0.0), Vec::<u32>::new());
        assert_eq!(run(&mut g, &mut t, 1, 0.0), vec![20]);
        assert_eq!(run(&mut g, &mut t, 31, 1.0), vec![30]);
        assert_eq!(run(&mut g, &mut t, 100, 0.0), Vec::<u32>::new());
    }

    #[test]
    fn in_between_readings_neither_step_down_nor_up() {
        let (mut g, mut t) = (PageFps::new(30), Instant::now());
        run(&mut g, &mut t, 4, 90.0);
        assert_eq!(run(&mut g, &mut t, 120, 6.0), Vec::<u32>::new());
        assert_eq!(g.fps(), 20);
    }

    #[test]
    fn a_low_configured_rate_only_uses_slower_steps() {
        assert_eq!(PageFps::new(15).levels, vec![15, 10]);
        assert_eq!(PageFps::new(10).levels, vec![10]);
        assert_eq!(PageFps::new(60).levels, vec![60, 30, 20, 15, 10]);
        let (mut g, mut t) = (PageFps::new(10), Instant::now());
        assert_eq!(run(&mut g, &mut t, 60, 90.0), Vec::<u32>::new());
    }
}
