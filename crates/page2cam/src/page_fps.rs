//! Makes the page cheaper while the output can't keep up.
//!
//! A heavy page (e.g. a WebGL animation) can take the whole GPU, and then
//! Chromium's work starves our compositor: the output skips most ticks. The
//! page is the one thing that can give way without touching the webcam or
//! the output rate, so [`PageQuality`] steps it down a ladder of two levers,
//! frame rate and render resolution, while the output skips ticks, and back
//! up once it has kept up for a while. The compositors scale a smaller page
//! up to the output; the overlay browser follows the frame rate only (its
//! text must stay sharp).

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

/// Render resolution, as a share of the configured page size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Res {
    /// The configured size (1920x1080).
    Full,
    /// Two thirds (1280x720 for 1080p).
    TwoThirds,
    /// Half (960x540 for 1080p).
    Half,
}

impl Res {
    /// The page size at this resolution for a `w` x `h` configured page
    /// (even numbers, at least 16).
    pub fn size(self, w: u32, h: u32) -> (u32, u32) {
        let (n, d) = match self {
            Res::Full => return (w, h),
            Res::TwoThirds => (2, 3),
            Res::Half => (1, 2),
        };
        let s = |v: u32| ((v as u64 * n / d) as u32 & !1).max(16);
        (s(w), s(h))
    }
}

/// One rung of the ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    pub fps: u32,
    pub res: Res,
}

/// Best to cheapest after the configured rate at full size (rungs faster
/// than the configured rate are left out).
const LADDER: [(u32, Res); 6] = [
    (30, Res::Full),
    (20, Res::Full),
    (15, Res::Full),
    (15, Res::TwoThirds),
    (10, Res::TwoThirds),
    (10, Res::Half),
];

/// Chooses the page frame rate and resolution from the output's skipped ticks.
#[derive(Debug)]
pub struct PageQuality {
    levels: Vec<Level>,
    at: usize,
    slow_since: Option<Instant>,
    ok_since: Option<Instant>,
    changed_at: Option<Instant>,
}

impl PageQuality {
    /// `max`: the configured frame rate (the page's normal rate).
    pub fn new(max: u32) -> Self {
        let max = max.max(1);
        let mut levels = vec![Level {
            fps: max,
            res: Res::Full,
        }];
        for (fps, res) in LADDER {
            if fps < max || (fps == max && res != Res::Full) {
                levels.push(Level { fps, res });
            }
        }
        Self {
            levels,
            at: 0,
            slow_since: None,
            ok_since: None,
            changed_at: None,
        }
    }

    /// The level now.
    pub fn level(&self) -> Level {
        self.levels[self.at]
    }

    /// Feed one reading (about once a second). Returns the new level when
    /// it changes.
    pub fn update(&mut self, now: Instant, skipped_pct: f32) -> Option<Level> {
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

    fn step(&mut self, now: Instant, to: usize) -> Level {
        self.at = to;
        self.changed_at = Some(now);
        self.slow_since = None;
        self.ok_since = None;
        self.level()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `pct` once a second for `secs` seconds from `*t`; the changes
    /// as (fps, size at 1920x1080).
    fn run(g: &mut PageQuality, t: &mut Instant, secs: u64, pct: f32) -> Vec<(u32, (u32, u32))> {
        let mut out = vec![];
        for _ in 0..secs {
            if let Some(l) = g.update(*t, pct) {
                out.push((l.fps, l.res.size(1920, 1080)));
            }
            *t += Duration::from_secs(1);
        }
        out
    }

    const FULL: (u32, u32) = (1920, 1080);
    const P720: (u32, u32) = (1280, 720);
    const P540: (u32, u32) = (960, 540);

    #[test]
    fn sizes_for_1080p_and_others() {
        assert_eq!(Res::Full.size(1920, 1080), FULL);
        assert_eq!(Res::TwoThirds.size(1920, 1080), P720);
        assert_eq!(Res::Half.size(1920, 1080), P540);
        assert_eq!(Res::TwoThirds.size(1280, 720), (852, 480));
        assert_eq!(Res::Half.size(20, 10), (16, 16));
    }

    #[test]
    fn keeps_the_configured_level_while_the_output_keeps_up() {
        let (mut g, mut t) = (PageQuality::new(30), Instant::now());
        assert_eq!(run(&mut g, &mut t, 120, 0.0), vec![]);
        assert_eq!(run(&mut g, &mut t, 120, 5.0), vec![]);
        assert_eq!(g.level(), Level { fps: 30, res: Res::Full });
    }

    #[test]
    fn walks_the_whole_ladder_down_one_step_per_settle() {
        let (mut g, mut t) = (PageQuality::new(30), Instant::now());
        // 3 s slow → first step; then one step per 10 s settle.
        assert_eq!(run(&mut g, &mut t, 4, 90.0), vec![(20, FULL)]);
        assert_eq!(run(&mut g, &mut t, 9, 90.0), vec![]);
        assert_eq!(run(&mut g, &mut t, 1, 90.0), vec![(15, FULL)]);
        assert_eq!(
            run(&mut g, &mut t, 60, 90.0),
            vec![(15, P720), (10, P720), (10, P540)]
        );
        assert_eq!(g.level(), Level { fps: 10, res: Res::Half }, "never below the last step");
    }

    #[test]
    fn a_short_spike_changes_nothing() {
        let (mut g, mut t) = (PageQuality::new(30), Instant::now());
        for _ in 0..10 {
            run(&mut g, &mut t, 2, 50.0);
            run(&mut g, &mut t, 1, 0.0);
        }
        assert_eq!(g.level().fps, 30);
    }

    #[test]
    fn steps_back_up_one_level_per_30_s() {
        let (mut g, mut t) = (PageQuality::new(30), Instant::now());
        run(&mut g, &mut t, 24, 90.0);
        assert_eq!(g.level(), Level { fps: 15, res: Res::TwoThirds });
        assert_eq!(run(&mut g, &mut t, 30, 0.0), vec![]);
        assert_eq!(run(&mut g, &mut t, 1, 0.0), vec![(15, FULL)]);
        assert_eq!(run(&mut g, &mut t, 31, 1.0), vec![(20, FULL)]);
        assert_eq!(run(&mut g, &mut t, 31, 2.0), vec![(30, FULL)]);
        assert_eq!(run(&mut g, &mut t, 100, 0.0), vec![]);
    }

    #[test]
    fn in_between_readings_neither_step_down_nor_up() {
        let (mut g, mut t) = (PageQuality::new(30), Instant::now());
        run(&mut g, &mut t, 4, 90.0);
        assert_eq!(run(&mut g, &mut t, 120, 6.0), vec![]);
        assert_eq!(g.level().fps, 20);
    }

    #[test]
    fn rungs_above_the_configured_rate_are_dropped() {
        let ladder = |max| -> Vec<(u32, Res)> {
            PageQuality::new(max).levels.iter().map(|l| (l.fps, l.res)).collect()
        };
        use Res::*;
        assert_eq!(
            ladder(30),
            [(30, Full), (20, Full), (15, Full), (15, TwoThirds), (10, TwoThirds), (10, Half)]
        );
        assert_eq!(
            ladder(60),
            [
                (60, Full),
                (30, Full),
                (20, Full),
                (15, Full),
                (15, TwoThirds),
                (10, TwoThirds),
                (10, Half)
            ]
        );
        assert_eq!(ladder(15), [(15, Full), (15, TwoThirds), (10, TwoThirds), (10, Half)]);
        assert_eq!(ladder(10), [(10, Full), (10, TwoThirds), (10, Half)]);
        assert_eq!(ladder(5), [(5, Full)]);
        let (mut g, mut t) = (PageQuality::new(5), Instant::now());
        assert_eq!(run(&mut g, &mut t, 60, 90.0), vec![]);
    }
}
