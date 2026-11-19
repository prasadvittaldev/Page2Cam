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

/// Smooths the raw level: a worse level must hold 6 s to show, a better one
/// 10 s to take over. Reasons follow the shown level live.
#[derive(Debug)]
pub struct Hysteresis {
    shown: Level,
    reasons: Vec<String>,
    worse_since: Option<Instant>,
    better_since: Option<Instant>,
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
            worse_since: None,
            better_since: None,
        }
    }

    pub fn level(&self) -> Level {
        self.shown
    }

    /// Back to Green (engine restarted/stopped).
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn update(
        &mut self,
        raw: Level,
        reasons: Vec<String>,
        now: Instant,
    ) -> (Level, Vec<String>) {
        use std::cmp::Ordering::*;
        match raw.cmp(&self.shown) {
            Equal => {
                self.worse_since = None;
                self.better_since = None;
                self.reasons = reasons;
            }
            Greater => {
                self.better_since = None;
                let since = *self.worse_since.get_or_insert(now);
                if now.duration_since(since) >= SHOW_AFTER {
                    self.shown = raw;
                    self.reasons = reasons;
                    self.worse_since = None;
                }
            }
            Less => {
                self.worse_since = None;
                let since = *self.better_since.get_or_insert(now);
                if now.duration_since(since) >= CLEAR_AFTER {
                    self.shown = raw;
                    self.reasons = reasons;
                    self.better_since = None;
                }
            }
        }
        (self.shown, self.reasons.clone())
    }
}

/// Windows' tray tooltip holds 127 UTF-16 units; we stay within 127 chars.
pub const TOOLTIP_MAX: usize = 127;

pub fn tooltip(state: &TrayState, level: Level, reasons: &[String]) -> String {
    let word = match state {
        TrayState::Stopped => "stopped".to_string(),
        TrayState::Starting { .. } => "starting…".to_string(),
        TrayState::Running => "running".to_string(),
        TrayState::Stopping { .. } => "stopping…".to_string(),
        TrayState::Error(m) => format!("error: {m}"),
    };
    let mut s = format!("Page2Cam — {word}");
    if !reasons.is_empty() && matches!(state, TrayState::Running) {
        let label = if level == Level::Red {
            "problem"
        } else {
            "strain"
        };
        s.push_str(&format!(" · {label}: {}", reasons.join(", ")));
    }
    if s.chars().count() > TOOLTIP_MAX {
        s = s.chars().take(TOOLTIP_MAX - 1).collect();
        s.push('…');
    }
    s
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
        let t = tooltip(&crate::TrayState::Running, Level::Green, &[]);
        assert_eq!(t, "Page2Cam — running");
        let t = tooltip(
            &crate::TrayState::Running,
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
        let t = tooltip(&crate::TrayState::Running, Level::Amber, &long);
        assert!(t.chars().count() <= 127, "{}", t.chars().count());
        assert!(t.chars().count() > 100);
        let t = tooltip(&crate::TrayState::Error("é".repeat(300)), Level::Red, &[]);
        assert!(t.chars().count() <= 127);
        assert!(tooltip(&crate::TrayState::Stopped, Level::Grey, &[]).contains("stopped"));
    }
}
