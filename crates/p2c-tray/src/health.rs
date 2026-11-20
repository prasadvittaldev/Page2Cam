//! Health classification, hysteresis and the tooltip.

use std::time::{Duration, Instant};

use p2c_admin::Health;

use crate::state::TrayState;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Stopped.
    Grey,
    Green,
    Amber,
    Red,
}

/// System readings from PDH; `None` = unknown (not strain).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SysLoad {
    /// `% Processor Utility`.
    pub cpu_utility_pct: Option<f32>,
    /// `% Processor Performance` (clock relative to base).
    pub clock_pct: Option<f32>,
}

pub const SHOW_AFTER: Duration = Duration::from_secs(6);
pub const CLEAR_AFTER: Duration = Duration::from_secs(10);
/// Samples are about 2 s apart; a longer silence means polling stopped or
/// failed, so running timers no longer describe a continuous reading.
pub const SAMPLE_GAP: Duration = Duration::from_secs(15);

/// Level and reasons for a running engine: Green, Amber (strain) or Red.
pub fn classify(h: &Health, sys: &SysLoad) -> (Level, Vec<String>) {
    let mut red = Vec::new();
    let cam = h.camera_status.as_str();
    if cam.contains("out of date") {
        red.push("camera driver out of date".to_string());
    } else if cam.starts_with("error") && cam.contains("not installed") {
        red.push("camera driver not installed".to_string());
    }
    if !red.is_empty() {
        return (Level::Red, red);
    }

    let mut r = Vec::new();
    if let Some(c) = sys.clock_pct.filter(|c| *c < 70.0) {
        r.push(format!("CPU throttled ({c:.0}% clock)"));
    }
    if let Some(c) = sys.cpu_utility_pct.filter(|c| *c > 85.0) {
        r.push(format!("CPU busy {c:.0}%"));
    }
    if h.output_skipped_pct > 2.0 {
        r.push(format!("output skipped {:.1}%", h.output_skipped_pct));
    }
    match h.webcam_state.as_str() {
        "reconnecting" => r.push("webcam reconnecting".into()),
        "error" => r.push("webcam error".into()),
        "ok" if h.webcam_fps < 12.0 => r.push(format!("webcam {:.0} fps", h.webcam_fps)),
        _ => {}
    }
    if h.cutout_auto_fallback {
        r.push("cut-out on Fast".into());
    }
    if h.video_fallback {
        r.push("video on CPU".into());
    }
    if cam.contains("retrying") {
        r.push("camera retrying".into());
    }
    let level = if r.is_empty() {
        Level::Green
    } else {
        Level::Amber
    };
    (level, r)
}

/// Smooths the raw level.
///
/// - A worse level shows only after that same level held continuously for
///   6 s (the timer restarts whenever the raw level differs).
/// - A better level takes over after 10 s of continuous readings that were
///   all better than the shown level; it becomes the worst raw level seen
///   during those 10 s (Red -> Amber -> Green takes two steps if Amber was
///   in the window).
/// - Reasons follow the shown level live.
/// - `no_sample()` (polling failed/stopped) or a gap of 15 s drops running
///   timers; `reset()` also goes back to Green.
#[derive(Debug)]
pub struct Hysteresis {
    shown: Level,
    reasons: Vec<String>,
    /// Worse level being waited for, and since when it held.
    pending: Option<(Level, Instant)>,
    /// Run of better-than-shown readings: start, worst level, its reasons.
    better: Option<(Instant, Level, Vec<String>)>,
    last_sample: Option<Instant>,
}

impl Default for Hysteresis {
    fn default() -> Self {
        Self::new()
    }
}

impl Hysteresis {
    pub fn new() -> Self {
        Self {
            shown: Level::Green,
            reasons: Vec::new(),
            pending: None,
            better: None,
            last_sample: None,
        }
    }

    pub fn level(&self) -> Level {
        self.shown
    }

    /// Back to Green (engine restarted/stopped).
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// No usable reading (poll failed, engine stopped answering): forget the
    /// running timers, keep what is shown.
    pub fn no_sample(&mut self) {
        self.pending = None;
        self.better = None;
        self.last_sample = None;
    }

    pub fn update(
        &mut self,
        raw: Level,
        reasons: Vec<String>,
        now: Instant,
    ) -> (Level, Vec<String>) {
        use std::cmp::Ordering::*;
        if self
            .last_sample
            .is_some_and(|t| now.duration_since(t) > SAMPLE_GAP)
        {
            self.no_sample();
        }
        self.last_sample = Some(now);
        match raw.cmp(&self.shown) {
            Equal => {
                self.pending = None;
                self.better = None;
                self.reasons = reasons;
            }
            Greater => {
                self.better = None;
                let since = match self.pending {
                    Some((l, t)) if l == raw => t,
                    _ => now,
                };
                self.pending = Some((raw, since));
                if now.duration_since(since) >= SHOW_AFTER {
                    self.shown = raw;
                    self.reasons = reasons;
                    self.pending = None;
                }
            }
            Less => {
                self.pending = None;
                let (start, worst, wr) = match self.better.take() {
                    Some((t, l, _)) if raw >= l => (t, raw, reasons),
                    Some((t, l, r)) => (t, l, r),
                    None => (now, raw, reasons),
                };
                if now.duration_since(start) >= CLEAR_AFTER {
                    self.shown = worst;
                    self.reasons = wr;
                } else {
                    self.better = Some((start, worst, wr));
                }
            }
        }
        (self.shown, self.reasons.clone())
    }
}

/// The level the icon shows. State-driven reds bypass hysteresis (immediate):
/// - Stopped, Starting, Stopping: Grey (not live yet / no more).
/// - Error: Red.
/// - Running and unresponsive: Red.
/// - Running otherwise: the hysteresis output.
pub fn effective_level(state: &TrayState, unresponsive: bool, hysteresis: Level) -> Level {
    match state {
        TrayState::Stopped | TrayState::Starting { .. } | TrayState::Stopping { .. } => Level::Grey,
        TrayState::Error(_) => Level::Red,
        TrayState::Running if unresponsive => Level::Red,
        TrayState::Running => hysteresis,
    }
}

/// Windows' tray tooltip holds 127 UTF-16 units; we stay within 127 chars.
pub const TOOLTIP_MAX: usize = 127;

/// `level` is the `effective_level`; `unresponsive` adds "not responding" as
/// the first reason while Running.
pub fn tooltip(state: &TrayState, unresponsive: bool, level: Level, reasons: &[String]) -> String {
    let word = match state {
        TrayState::Stopped => "stopped".to_string(),
        TrayState::Starting { .. } => "starting…".to_string(),
        TrayState::Running => "running".to_string(),
        TrayState::Stopping { .. } => "stopping…".to_string(),
        TrayState::Error(m) => format!("error: {m}"),
    };
    let mut s = format!("Page2Cam — {word}");
    if matches!(state, TrayState::Running) {
        let mut all: Vec<&str> = Vec::new();
        if unresponsive {
            all.push("not responding");
        }
        all.extend(reasons.iter().map(String::as_str));
        if !all.is_empty() {
            let label = if level == Level::Red {
                "problem"
            } else {
                "strain"
            };
            s.push_str(&format!(" · {label}: {}", all.join(", ")));
        }
    }
    clip(s)
}

fn clip(mut s: String) -> String {
    if s.chars().count() > TOOLTIP_MAX {
        s = s.chars().take(TOOLTIP_MAX - 1).collect();
        s.push('…');
    }
    s
}

/// `tip` with `line` added on a new line, `tip` shortened so `line` always fits.
pub fn append_line(tip: &str, line: &str) -> String {
    let room = TOOLTIP_MAX.saturating_sub(line.chars().count() + 1);
    let mut base: String = tip.chars().take(room).collect();
    if base.chars().count() < tip.chars().count() && room > 0 {
        base = base.chars().take(room - 1).collect();
        base.push('…');
    }
    clip(format!("{base}\n{line}"))
}

/// What the icon and tooltip show: [`effective_level`] and [`tooltip`],
/// except that a camera problem the tray found itself (driver not installed
/// or out of date) makes the icon red in every state and leads the tooltip.
pub fn display(
    state: &TrayState,
    unresponsive: bool,
    hysteresis: Level,
    reasons: &[String],
    camera_problem: Option<&str>,
) -> (Level, String) {
    let level = effective_level(state, unresponsive, hysteresis);
    let Some(p) = camera_problem else {
        return (level, tooltip(state, unresponsive, level, reasons));
    };
    if matches!(state, TrayState::Running) {
        let mut all = vec![p.to_string()];
        all.extend(reasons.iter().filter(|r| r.as_str() != p).cloned());
        return (Level::Red, tooltip(state, unresponsive, Level::Red, &all));
    }
    let s = format!("{} · problem: {p}", tooltip(state, false, Level::Red, &[]));
    (Level::Red, clip(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2c_admin::Health;
    use std::time::{Duration, Instant};

    fn ok() -> Health {
        Health {
            output_ready: true,
            webcam_fps: 30.0,
            webcam_state: "ok".into(),
            camera_status: "ok: Windows camera \"Page2Cam\"".into(),
            ..Health::default()
        }
    }
    fn calm() -> SysLoad {
        SysLoad {
            cpu_utility_pct: Some(40.0),
            clock_pct: Some(100.0),
        }
    }

    #[test]
    fn classify_table() {
        let (l, r) = classify(&ok(), &calm());
        assert_eq!((l, r.len()), (Level::Green, 0));

        type Mutate = fn(&mut Health);
        let amber: &[(Mutate, &str)] = &[
            (|h| h.output_skipped_pct = 2.5, "skipped"),
            (|h| h.webcam_fps = 9.0, "9 fps"),
            (|h| h.webcam_state = "reconnecting".into(), "reconnecting"),
            (|h| h.webcam_state = "error".into(), "webcam error"),
            (|h| h.cutout_auto_fallback = true, "Fast"),
            (|h| h.video_fallback = true, "CPU"),
            (
                |h| h.camera_status = "starting (retrying): x".into(),
                "camera retrying",
            ),
        ];
        for (m, needle) in amber {
            let mut h = ok();
            m(&mut h);
            let (l, r) = classify(&h, &calm());
            assert_eq!(l, Level::Amber, "{needle}");
            assert!(r.iter().any(|x| x.contains(needle)), "{needle}: {r:?}");
        }
        // exactly 2 % is not "more than 2 %"
        let mut h = ok();
        h.output_skipped_pct = 2.0;
        assert_eq!(classify(&h, &calm()).0, Level::Green);

        let busy = SysLoad {
            cpu_utility_pct: Some(90.0),
            clock_pct: Some(100.0),
        };
        let (l, r) = classify(&ok(), &busy);
        assert_eq!(l, Level::Amber);
        assert!(r[0].contains("CPU"), "{r:?}");
        let slow = SysLoad {
            cpu_utility_pct: Some(10.0),
            clock_pct: Some(60.0),
        };
        let (l, r) = classify(&ok(), &slow);
        assert_eq!(l, Level::Amber);
        assert!(r[0].contains("throttled"), "{r:?}");
        // unknown load (PDH failed) is not strain
        assert_eq!(classify(&ok(), &SysLoad::default()).0, Level::Green);
    }

    #[test]
    fn webcam_off_is_not_low_fps() {
        let mut h = ok();
        h.webcam_state = "off".into();
        h.webcam_fps = 0.0;
        assert_eq!(classify(&h, &calm()).0, Level::Green);
        // an older engine without the field (empty state) is not judged either
        h.webcam_state = String::new();
        assert_eq!(classify(&h, &calm()).0, Level::Green);
    }

    #[test]
    fn red_reasons() {
        for s in [
            "error: camera driver out of date — reinstall Page2Cam",
            "error: the Page2Cam camera is not installed. Close Page2Cam",
        ] {
            let mut h = ok();
            h.camera_status = s.into();
            let (l, r) = classify(&h, &calm());
            assert_eq!(l, Level::Red, "{s}");
            assert!(!r.is_empty());
        }
    }

    #[test]
    fn health_json_from_engine_deserializes() {
        let j = r#"{"output_ready":true,"output_skipped_pct":0.0,"webcam_fps":0.0,"webcam_state":"off","cutout_auto_fallback":false,"video_fallback":false,"camera_status":"ok"}"#;
        let h: Health = serde_json::from_str(j).unwrap();
        assert_eq!(h.webcam_state, "off");
        let h: Health = serde_json::from_str("{}").unwrap();
        assert_eq!(classify(&h, &calm()).0, Level::Green);
    }

    #[test]
    fn amber_needs_6s_and_clears_after_10s() {
        let t = Instant::now();
        let at = |n: u64| t + Duration::from_secs(n);
        let mut hy = Hysteresis::new();
        let bad = |hy: &mut Hysteresis, n| hy.update(Level::Amber, vec!["x".into()], at(n));
        let good = |hy: &mut Hysteresis, n| hy.update(Level::Green, vec![], at(n));
        assert_eq!(good(&mut hy, 0).0, Level::Green);
        assert_eq!(bad(&mut hy, 1).0, Level::Green);
        assert_eq!(bad(&mut hy, 6).0, Level::Green); // 5 s
        assert_eq!(bad(&mut hy, 7).0, Level::Amber); // 6 s
                                                     // good readings clear only after 10 s of continuous good
        assert_eq!(good(&mut hy, 8).0, Level::Amber);
        assert_eq!(good(&mut hy, 17).0, Level::Amber); // 9 s
                                                       // a bad blip resets the clear timer
        assert_eq!(bad(&mut hy, 18).0, Level::Amber);
        assert_eq!(good(&mut hy, 19).0, Level::Amber);
        assert_eq!(good(&mut hy, 28).0, Level::Amber); // 9 s
        let (l, r) = good(&mut hy, 29);
        assert_eq!(l, Level::Green);
        assert!(r.is_empty());
    }

    fn up(hy: &mut Hysteresis, l: Level, n: u64, t: Instant) -> Level {
        hy.update(l, vec![format!("{l:?}")], t + Duration::from_secs(n))
            .0
    }

    #[test]
    fn red_timer_restarts_when_level_changes() {
        let t = Instant::now();
        let mut hy = Hysteresis::new();
        assert_eq!(up(&mut hy, Level::Amber, 0, t), Level::Green);
        // red appears at 3: must hold 6 s itself (until 9), not 3 s (6)
        assert_eq!(up(&mut hy, Level::Red, 3, t), Level::Green);
        assert_eq!(up(&mut hy, Level::Red, 6, t), Level::Green);
        assert_eq!(up(&mut hy, Level::Red, 8, t), Level::Green);
        assert_eq!(up(&mut hy, Level::Red, 9, t), Level::Red);
    }

    #[test]
    fn red_clears_to_worst_of_last_10s_not_green() {
        let t = Instant::now();
        let mut hy = Hysteresis::new();
        up(&mut hy, Level::Red, 0, t);
        assert_eq!(up(&mut hy, Level::Red, 6, t), Level::Red);
        // amber for 9 s, then one green sample
        for n in [7, 9, 11, 13, 15] {
            assert_eq!(up(&mut hy, Level::Amber, n, t), Level::Red);
        }
        assert_eq!(up(&mut hy, Level::Green, 16, t), Level::Red);
        // 10 s after the first better reading (7): Amber, not Green
        let (l, r) = hy.update(Level::Green, vec![], t + Duration::from_secs(17));
        assert_eq!(l, Level::Amber);
        assert_eq!(r, vec!["Amber".to_string()]);
        // and Amber needs its own 10 s of good to clear
        assert_eq!(up(&mut hy, Level::Green, 26, t), Level::Amber);
        assert_eq!(up(&mut hy, Level::Green, 35, t), Level::Amber);
        assert_eq!(up(&mut hy, Level::Green, 36, t), Level::Green);
    }

    #[test]
    fn long_gap_drops_stale_timers() {
        let t = Instant::now();
        let mut hy = Hysteresis::new();
        up(&mut hy, Level::Amber, 0, t);
        up(&mut hy, Level::Amber, 4, t);
        // 100 s without samples, then one amber: must not show at once
        assert_eq!(up(&mut hy, Level::Amber, 104, t), Level::Green);
        assert_eq!(up(&mut hy, Level::Amber, 110, t), Level::Amber);
        // same on the clear side
        up(&mut hy, Level::Green, 111, t);
        assert_eq!(up(&mut hy, Level::Green, 200, t), Level::Amber);
        assert_eq!(up(&mut hy, Level::Green, 209, t), Level::Amber);
        assert_eq!(up(&mut hy, Level::Green, 210, t), Level::Green);
    }

    #[test]
    fn no_sample_drops_timers() {
        let t = Instant::now();
        let mut hy = Hysteresis::new();
        up(&mut hy, Level::Amber, 0, t);
        up(&mut hy, Level::Amber, 4, t);
        hy.no_sample();
        assert_eq!(up(&mut hy, Level::Amber, 6, t), Level::Green);
        assert_eq!(up(&mut hy, Level::Amber, 12, t), Level::Amber);
        up(&mut hy, Level::Green, 13, t);
        hy.no_sample();
        assert_eq!(up(&mut hy, Level::Green, 23, t), Level::Amber);
    }

    #[test]
    fn effective_level_state_driven() {
        use crate::TrayState::*;
        let t = Instant::now();
        assert_eq!(effective_level(&Stopped, false, Level::Amber), Level::Grey);
        assert_eq!(
            effective_level(&Starting { since: t }, false, Level::Green),
            Level::Grey
        );
        assert_eq!(
            effective_level(&Error("x".into()), false, Level::Green),
            Level::Red
        );
        assert_eq!(effective_level(&Running, true, Level::Green), Level::Red);
        assert_eq!(effective_level(&Running, false, Level::Amber), Level::Amber);
    }

    #[test]
    fn tooltip_shows_unresponsive_as_problem() {
        let t = tooltip(&crate::TrayState::Running, true, Level::Red, &[]);
        assert_eq!(t, "Page2Cam — running · problem: not responding");
    }

    #[test]
    fn flapping_never_shows() {
        let t = Instant::now();
        let mut hy = Hysteresis::new();
        for i in 0..40u64 {
            let at = t + Duration::from_secs(i * 2);
            let raw = if i % 2 == 0 {
                Level::Amber
            } else {
                Level::Green
            };
            let r = if raw == Level::Amber {
                vec!["x".to_string()]
            } else {
                vec![]
            };
            assert_eq!(hy.update(raw, r, at).0, Level::Green, "i={i}");
        }
    }

    #[test]
    fn amber_to_red_needs_6s_and_reset_goes_green() {
        let t = Instant::now();
        let at = |n: u64| t + Duration::from_secs(n);
        let mut hy = Hysteresis::new();
        hy.update(Level::Amber, vec!["a".into()], at(0));
        hy.update(Level::Amber, vec!["a".into()], at(6));
        assert_eq!(hy.level(), Level::Amber);
        hy.update(Level::Red, vec!["r".into()], at(7));
        assert_eq!(
            hy.update(Level::Red, vec!["r".into()], at(12)).0,
            Level::Amber
        );
        assert_eq!(
            hy.update(Level::Red, vec!["r".into()], at(13)).0,
            Level::Red
        );
        hy.reset();
        assert_eq!(hy.level(), Level::Green);
    }

    #[test]
    fn tooltip_truncates() {
        let t = tooltip(&crate::TrayState::Running, false, Level::Green, &[]);
        assert_eq!(t, "Page2Cam — running");
        let t = tooltip(
            &crate::TrayState::Running,
            false,
            Level::Amber,
            &["CPU throttled".into(), "webcam 9 fps".into()],
        );
        assert_eq!(
            t,
            "Page2Cam — running · strain: CPU throttled, webcam 9 fps"
        );
        let long: Vec<String> = (0..20)
            .map(|i| format!("a very long reason number {i}"))
            .collect();
        let t = tooltip(&crate::TrayState::Running, false, Level::Amber, &long);
        assert!(t.chars().count() <= 127, "{}", t.chars().count());
        assert!(t.chars().count() > 100);
        let t = tooltip(
            &crate::TrayState::Error("é".repeat(300)),
            false,
            Level::Red,
            &[],
        );
        assert!(t.chars().count() <= 127);
        assert!(tooltip(&crate::TrayState::Stopped, false, Level::Grey, &[]).contains("stopped"));
    }

    #[test]
    fn camera_problem_is_red_in_every_state() {
        use crate::TrayState::*;
        let p = Some("camera driver not installed");
        let (l, t) = display(&Stopped, false, Level::Green, &[], p);
        assert_eq!(l, Level::Red);
        assert!(t.contains("stopped") && t.contains("not installed"), "{t}");
        let (l, t) = display(&Running, false, Level::Green, &["webcam 9 fps".into()], p);
        assert_eq!(l, Level::Red);
        assert!(
            t.contains("problem: camera driver not installed, webcam 9 fps"),
            "{t}"
        );
        // No problem: same as effective_level + tooltip.
        let (l, t) = display(&Stopped, false, Level::Amber, &[], None);
        assert_eq!(l, Level::Grey);
        assert_eq!(t, tooltip(&Stopped, false, Level::Grey, &[]));
    }
}
