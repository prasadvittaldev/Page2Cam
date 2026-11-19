//! Tray state machine.
//!
//! `Supervisor` owns the state plus the little memory the crash policy needs.
//! The Windows shell feeds it [`Event`]s and carries out the returned
//! [`Action`]s in order. Pure: no clocks, no I/O (time is passed in).
//!
//! ```text
//! Stopped --Start--> Starting --Ready--> Running --Stop--> Stopping --exit--> Stopped
//!   ^                  |  | 30 s            |  crash (1st in 2 min) -> Starting (auto restart)
//!   |                  |  v                 |  crash (2nd in 2 min) -> Error
//!   +--- exit 0 -------+ Error  <-- 76 twice, 78, start timeout, restart loop
//!                      exit 75 -> Starting (restart, own loop counter)
//!                      exit 76 (Starting only) -> Starting once more on a new port
//! Force restart: any state -> Starting (kill tree, clean leftovers,
//!                re-register camera, spawn).
//! Stopping: PostQuit; after 10 s KillEngineTree; any exit -> Stopped.
//! ```
//!
//! Generations: every `SpawnEngine` carries a fresh `gen`; the shell tags the
//! events of that engine (`EngineReady`, `EngineExited`, `EngineUnresponsive`)
//! with it. Events of any other (killed, replaced) engine are dropped, so the
//! exit of an engine we killed ourselves can never count as a crash or start a
//! second engine.
//!
//! Exit 75 (restart requested) has its own counter: more than 3 within 120 s
//! -> Error("engine keeps restarting"); a 75 received while still Starting
//! counts double. The counter resets once an engine ran for 60 s.
//!
//! 76 and 78 are only special while Starting; while Running they are crashes.

use std::time::{Duration, Instant};

use p2c_shm::Writer;

/// Engine start timeout (no `output_ready` by then -> Error).
pub const START_TIMEOUT: Duration = Duration::from_secs(30);
/// After `PostQuit`, wait this long before killing the engine tree.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// Two crashes within this window -> Error.
pub const CRASH_WINDOW: Duration = Duration::from_secs(120);
/// Window of the exit-75 loop guard.
pub const RESTART_WINDOW: Duration = Duration::from_secs(120);
/// More than this (weighted) 75-restarts within the window -> Error.
pub const RESTART_LIMIT: u32 = 3;
/// An engine that ran this long clears the 75 counter.
pub const STABLE_AFTER: Duration = Duration::from_secs(60);
/// Pause between `SetWriter(Tray)` and the first standby write, so an engine
/// write that was in flight cannot overlap it.
pub const STANDBY_GRACE: Duration = Duration::from_millis(100);

/// Engine exit codes (see `crates/page2cam/src/engine.rs`).
pub const EXIT_RESTART: i32 = 75;
pub const EXIT_PORT: i32 = 76;
pub const EXIT_CONFIG: i32 = 78;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayState {
    Stopped,
    Starting {
        since: Instant,
    },
    Running,
    Stopping {
        since: Instant,
    },
    /// Red; the engine is not running. Start / Force restart leave it.
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    StartClicked,
    StopClicked,
    ForceRestartClicked,
    /// `/api/state` reports `health.output_ready` (first answer while Starting;
    /// later answers after an unresponsive spell). `gen` = the engine's
    /// generation from its `SpawnEngine`.
    EngineReady {
        gen: u64,
    },
    /// The engine process ended with this code.
    EngineExited {
        gen: u64,
        code: i32,
    },
    /// No answer from the engine for 5 s while Running.
    EngineUnresponsive {
        gen: u64,
    },
    /// Periodic (about 1 s) for timeouts.
    Tick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Start the engine; `new_port` = choose a fresh admin port first. Tag
    /// all its events with `gen`.
    SpawnEngine {
        new_port: bool,
        gen: u64,
    },
    /// `POST /api/quit`.
    PostQuit,
    KillEngineTree,
    /// Stop other Page2Cam/Chromium leftovers.
    StopLeftovers,
    ReRegisterCamera,
    /// Set the shared-memory `writer` field.
    SetWriter(Writer),
    /// Resume standby writes (~1 fps) after this grace period.
    ResumeStandbyAfter(Duration),
    /// Balloon / log message.
    Notify(String),
}

#[derive(Debug)]
pub struct Supervisor {
    state: TrayState,
    gen: u64,
    last_crash: Option<Instant>,
    /// (when, weight) of recent exit-75 restarts.
    restarts: Vec<(Instant, u32)>,
    running_since: Option<Instant>,
    port_retried: bool,
    kill_sent: bool,
    unresponsive: bool,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

fn take_back() -> [Action; 2] {
    [
        Action::SetWriter(Writer::Tray),
        Action::ResumeStandbyAfter(STANDBY_GRACE),
    ]
}

impl Supervisor {
    pub fn new() -> Self {
        Self {
            state: TrayState::Stopped,
            gen: 0,
            last_crash: None,
            restarts: Vec::new(),
            running_since: None,
            port_retried: false,
            kill_sent: false,
            unresponsive: false,
        }
    }

    pub fn state(&self) -> &TrayState {
        &self.state
    }

    /// Generation of the engine currently tracked (0 before the first spawn).
    pub fn generation(&self) -> u64 {
        self.gen
    }

    /// Running but the engine has not answered for 5 s: the icon is red
    /// (state stays Running; the operator decides: Force restart). See
    /// `health::effective_level`.
    pub fn is_unresponsive(&self) -> bool {
        self.unresponsive
    }

    fn fresh(&mut self) {
        self.last_crash = None;
        self.restarts.clear();
        self.running_since = None;
        self.port_retried = false;
        self.kill_sent = false;
        self.unresponsive = false;
    }

    fn spawn(&mut self, new_port: bool) -> Action {
        self.gen += 1;
        Action::SpawnEngine {
            new_port,
            gen: self.gen,
        }
    }

    fn fail(&mut self, msg: String, mut actions: Vec<Action>) -> Vec<Action> {
        self.unresponsive = false;
        self.running_since = None;
        actions.extend(take_back());
        actions.push(Action::Notify(msg.clone()));
        self.state = TrayState::Error(msg);
        actions
    }

    pub fn on_event(&mut self, ev: Event, now: Instant) -> Vec<Action> {
        use TrayState::*;
        // Events of another (killed or replaced) engine are stale.
        match ev {
            Event::EngineReady { gen }
            | Event::EngineExited { gen, .. }
            | Event::EngineUnresponsive { gen }
                if gen != self.gen =>
            {
                return Vec::new();
            }
            _ => {}
        }
        match (ev, self.state.clone()) {
            (Event::StartClicked, Stopped | Error(_)) => {
                self.fresh();
                self.state = Starting { since: now };
                let spawn = self.spawn(false);
                vec![Action::SetWriter(Writer::Tray), spawn]
            }
            (Event::ForceRestartClicked, _) => {
                self.fresh();
                self.state = Starting { since: now };
                let spawn = self.spawn(false);
                vec![
                    Action::SetWriter(Writer::Tray),
                    Action::KillEngineTree,
                    Action::StopLeftovers,
                    Action::ReRegisterCamera,
                    Action::ResumeStandbyAfter(STANDBY_GRACE),
                    spawn,
                ]
            }
            (Event::StopClicked, Starting { .. } | Running) => {
                self.kill_sent = false;
                self.unresponsive = false;
                self.state = Stopping { since: now };
                // Take the camera back first, so standby shows while the engine winds down.
                let mut a = take_back().to_vec();
                a.push(Action::PostQuit);
                a
            }
            (Event::EngineReady { .. }, Starting { .. }) => {
                self.state = Running;
                self.running_since = Some(now);
                vec![Action::SetWriter(Writer::Engine)]
            }
            (Event::EngineReady { .. }, Running) => {
                self.unresponsive = false;
                Vec::new()
            }
            (Event::EngineUnresponsive { .. }, Running) if !self.unresponsive => {
                self.unresponsive = true;
                vec![Action::Notify(
                    "Page2Cam is not responding - use Force restart".into(),
                )]
            }
            (Event::Tick, Starting { since }) if now.duration_since(since) >= START_TIMEOUT => self
                .fail(
                    "engine did not start within 30 s".into(),
                    vec![Action::KillEngineTree, Action::StopLeftovers],
                ),
            (Event::Tick, Stopping { since })
                if !self.kill_sent && now.duration_since(since) >= STOP_TIMEOUT =>
            {
                self.kill_sent = true;
                vec![Action::KillEngineTree]
            }
            (Event::EngineExited { .. }, Stopping { .. }) => {
                let mut a = take_back().to_vec();
                if self.kill_sent {
                    a.push(Action::StopLeftovers);
                }
                self.kill_sent = false;
                self.running_since = None;
                self.state = Stopped;
                a
            }
            (Event::EngineExited { code, .. }, Starting { .. } | Running) => self.exited(code, now),
            _ => Vec::new(),
        }
    }

    fn restart(&mut self, now: Instant, new_port: bool, mut pre: Vec<Action>) -> Vec<Action> {
        self.unresponsive = false;
        self.running_since = None;
        self.state = TrayState::Starting { since: now };
        pre.push(self.spawn(new_port));
        pre
    }

    fn exited(&mut self, code: i32, now: Instant) -> Vec<Action> {
        let was_starting = matches!(self.state, TrayState::Starting { .. });
        if self
            .running_since
            .is_some_and(|t| now.duration_since(t) >= STABLE_AFTER)
        {
            self.restarts.clear();
        }
        match code {
            EXIT_RESTART => {
                self.restarts
                    .retain(|(t, _)| now.duration_since(*t) < RESTART_WINDOW);
                self.restarts.push((now, if was_starting { 2 } else { 1 }));
                if self.restarts.iter().map(|(_, w)| w).sum::<u32>() > RESTART_LIMIT {
                    self.fail("engine keeps restarting".into(), Vec::new())
                } else {
                    self.restart(now, false, take_back().to_vec())
                }
            }
            EXIT_PORT if was_starting && !self.port_retried => {
                self.port_retried = true;
                self.restart(now, true, take_back().to_vec())
            }
            EXIT_PORT if was_starting => self.fail("admin port unavailable".into(), Vec::new()),
            EXIT_CONFIG if was_starting => self.fail(
                "engine misconfigured (admin token missing or invalid) - please report this".into(),
                Vec::new(),
            ),
            0 => {
                self.unresponsive = false;
                self.running_since = None;
                self.state = TrayState::Stopped;
                let mut a = take_back().to_vec();
                a.push(Action::Notify("Page2Cam stopped".into()));
                a
            }
            code => {
                let again = self
                    .last_crash
                    .is_some_and(|t| now.duration_since(t) < CRASH_WINDOW);
                if again {
                    self.fail(
                        format!("engine crashed twice within 2 minutes (exit code {code})"),
                        vec![Action::StopLeftovers],
                    )
                } else {
                    self.last_crash = Some(now);
                    let mut a = take_back().to_vec();
                    a.push(Action::StopLeftovers);
                    a.push(Action::Notify("Page2Cam crashed - restarting".into()));
                    self.restart(now, false, a)
                }
            }
        }
    }
}

/// What the shell does with an engine's exit before the supervisor sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitHandling {
    /// Pass it to [`Supervisor::on_event`].
    Feed,
    /// An engine the tray killed itself: it only completes a Stop, never a
    /// crash, so it is dropped unless Stopping.
    IgnoreKilled,
    /// Windows is ending the session (the engine got WM_ENDSESSION too):
    /// no restart, no crash balloon.
    IgnoreSignOut,
}

/// Decide [`ExitHandling`] for an exit: `killed` = the tray ended that
/// engine, `stopping` = the state is Stopping, `session_ending` = Windows
/// asked to end the session (WM_QUERYENDSESSION).
pub fn exit_handling(killed: bool, stopping: bool, session_ending: bool) -> ExitHandling {
    if session_ending {
        ExitHandling::IgnoreSignOut
    } else if killed && !stopping {
        ExitHandling::IgnoreKilled
    } else {
        ExitHandling::Feed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn t0() -> Instant {
        Instant::now()
    }
    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }
    fn started(sup: &mut Supervisor, now: Instant) {
        sup.on_event(Event::StartClicked, now);
    }
    fn has(a: &[Action], x: &Action) -> bool {
        a.contains(x)
    }
    fn is_spawn(x: &Action) -> bool {
        matches!(x, Action::SpawnEngine { .. })
    }
    fn ready(sup: &mut Supervisor, now: Instant) -> Vec<Action> {
        sup.on_event(
            Event::EngineReady {
                gen: sup.generation(),
            },
            now,
        )
    }
    fn unresp(sup: &mut Supervisor, now: Instant) -> Vec<Action> {
        sup.on_event(
            Event::EngineUnresponsive {
                gen: sup.generation(),
            },
            now,
        )
    }
    fn exited(sup: &mut Supervisor, code: i32, now: Instant) -> Vec<Action> {
        sup.on_event(
            Event::EngineExited {
                gen: sup.generation(),
                code,
            },
            now,
        )
    }

    #[test]
    fn stale_exit_after_force_restart_is_ignored() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        ready(&mut sup, now);
        let old = sup.generation();
        let a = sup.on_event(Event::ForceRestartClicked, now + s(5));
        assert_eq!(sup.generation(), old + 1);
        assert!(a.contains(&Action::SpawnEngine {
            new_port: false,
            gen: old + 1
        }));
        // The killed engine's exit arrives late: no crash, no second spawn, no notify.
        let a = sup.on_event(Event::EngineExited { gen: old, code: 1 }, now + s(6));
        assert!(a.is_empty(), "{a:?}");
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
        // Stale ready/unresponsive are dropped too.
        assert!(sup
            .on_event(Event::EngineReady { gen: old }, now + s(6))
            .is_empty());
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
        // The new engine still works and a later real crash is the first one.
        ready(&mut sup, now + s(8));
        exited(&mut sup, 1, now + s(20));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
    }

    #[test]
    fn stale_exit_after_start_timeout_and_restart_is_ignored() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        let old = sup.generation();
        sup.on_event(Event::Tick, now + s(30));
        assert!(matches!(sup.state(), TrayState::Error(_)));
        // exit of the killed engine in Error: ignored
        assert!(exited(&mut sup, 1, now + s(31)).is_empty());
        started(&mut sup, now + s(40));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
        let a = sup.on_event(Event::EngineExited { gen: old, code: 1 }, now + s(41));
        assert!(a.is_empty(), "{a:?}");
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
    }

    #[test]
    fn exit_75_loop_is_capped() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        ready(&mut sup, now);
        for i in 0..3 {
            exited(&mut sup, 75, now + s(10 + i));
            assert!(matches!(sup.state(), TrayState::Starting { .. }), "i={i}");
            ready(&mut sup, now + s(10 + i));
        }
        let a = exited(&mut sup, 75, now + s(14));
        match sup.state() {
            TrayState::Error(m) => assert_eq!(m, "engine keeps restarting"),
            o => panic!("{o:?}"),
        }
        assert!(!a.iter().any(is_spawn));
    }

    #[test]
    fn exit_75_while_starting_counts_double() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        exited(&mut sup, 75, now + s(1));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
        exited(&mut sup, 75, now + s(2));
        assert!(matches!(sup.state(), TrayState::Error(_)));
    }

    #[test]
    fn exit_75_counter_resets_after_stable_run_and_window() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        ready(&mut sup, now);
        exited(&mut sup, 75, now + s(10));
        exited(&mut sup, 75, now + s(11)); // Starting: weight 2 -> total 3
        ready(&mut sup, now + s(12));
        // ran 60 s: counter cleared, so this 75 is the first again
        exited(&mut sup, 75, now + s(72));
        ready(&mut sup, now + s(73));
        exited(&mut sup, 75, now + s(74));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
        // old entries expire after 120 s even without a stable run
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        ready(&mut sup, now);
        for i in 0..3 {
            exited(&mut sup, 75, now + s(i));
            ready(&mut sup, now + s(i));
        }
        exited(&mut sup, 75, now + s(123));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
    }

    #[test]
    fn exit_76_and_78_while_running_are_crashes() {
        for code in [76, 78] {
            let now = t0();
            let mut sup = Supervisor::new();
            started(&mut sup, now);
            ready(&mut sup, now);
            let a = exited(&mut sup, code, now + s(5));
            assert!(
                matches!(sup.state(), TrayState::Starting { .. }),
                "{code}: {:?}",
                sup.state()
            );
            assert!(a.iter().any(|x| matches!(
                x,
                Action::SpawnEngine {
                    new_port: false,
                    ..
                }
            )));
        }
    }
    #[test]
    fn start_stop_happy_path() {
        let now = t0();
        let mut sup = Supervisor::new();
        assert_eq!(*sup.state(), TrayState::Stopped);
        let a = sup.on_event(Event::StartClicked, now);
        assert_eq!(*sup.state(), TrayState::Starting { since: now });
        assert!(a.iter().any(is_spawn));
        let a = ready(&mut sup, now + s(2));
        assert_eq!(*sup.state(), TrayState::Running);
        assert_eq!(a, vec![Action::SetWriter(p2c_shm::Writer::Engine)]);
        let a = sup.on_event(Event::StopClicked, now + s(60));
        assert_eq!(*sup.state(), TrayState::Stopping { since: now + s(60) });
        assert!(has(&a, &Action::PostQuit));
        exited(&mut sup, 0, now + s(62));
        assert_eq!(*sup.state(), TrayState::Stopped);
    }

    #[test]
    fn stop_timeout_kills_tree() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        ready(&mut sup, now);
        sup.on_event(Event::StopClicked, now + s(1));
        assert!(sup.on_event(Event::Tick, now + s(10)).is_empty());
        let a = sup.on_event(Event::Tick, now + s(11));
        assert_eq!(a, vec![Action::KillEngineTree]);
        // only once
        assert!(sup.on_event(Event::Tick, now + s(12)).is_empty());
        let a = exited(&mut sup, 1, now + s(12));
        assert_eq!(*sup.state(), TrayState::Stopped);
        assert!(has(&a, &Action::StopLeftovers));
        assert!(has(&a, &Action::SetWriter(p2c_shm::Writer::Tray)));
    }

    #[test]
    fn crash_restarts_once_then_error() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        ready(&mut sup, now);
        let a = exited(&mut sup, 1, now + s(10));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
        assert!(a.iter().any(is_spawn));
        assert!(a.iter().any(|x| matches!(x, Action::Notify(_))));
        ready(&mut sup, now + s(12));
        let a = exited(&mut sup, 0xC0000005u32 as i32, now + s(60));
        assert!(matches!(sup.state(), TrayState::Error(_)));
        assert!(!a.iter().any(|x| matches!(x, Action::SpawnEngine { .. })));
        assert!(has(&a, &Action::SetWriter(p2c_shm::Writer::Tray)));
        // Start clicked in Error starts fresh.
        let a = sup.on_event(Event::StartClicked, now + s(70));
        assert!(a.iter().any(is_spawn));
    }

    #[test]
    fn crash_after_two_minutes_is_a_fresh_first_crash() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        ready(&mut sup, now);
        exited(&mut sup, 1, now + s(10));
        ready(&mut sup, now + s(12));
        exited(&mut sup, 1, now + s(10 + 121));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
    }

    #[test]
    fn exit_75_restarts_without_counting() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        ready(&mut sup, now);
        for i in 0..3 {
            let a = exited(&mut sup, 75, now + s(10 + i));
            assert!(a.iter().any(is_spawn));
            assert!(matches!(sup.state(), TrayState::Starting { .. }));
            ready(&mut sup, now + s(10 + i));
        }
        // A later crash is still the first one: restarted, not Error.
        exited(&mut sup, 1, now + s(20));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
    }

    #[test]
    fn exit_76_retries_new_port_once() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        let a = exited(&mut sup, 76, now + s(1));
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::SpawnEngine { new_port: true, .. })));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
        let a = exited(&mut sup, 76, now + s(2));
        assert!(matches!(sup.state(), TrayState::Error(_)));
        assert!(!a.iter().any(|x| matches!(x, Action::SpawnEngine { .. })));
    }

    #[test]
    fn exit_78_is_error_without_restart() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        let a = exited(&mut sup, 78, now + s(1));
        match sup.state() {
            TrayState::Error(m) => assert!(m.contains("engine misconfigured"), "{m}"),
            o => panic!("{o:?}"),
        }
        assert!(!a.iter().any(|x| matches!(x, Action::SpawnEngine { .. })));
    }

    #[test]
    fn exit_0_unrequested_is_stopped_no_restart() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        ready(&mut sup, now);
        let a = exited(&mut sup, 0, now + s(5));
        assert_eq!(*sup.state(), TrayState::Stopped);
        assert!(!a.iter().any(|x| matches!(x, Action::SpawnEngine { .. })));
    }

    #[test]
    fn exit_while_stopping_is_always_normal() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        ready(&mut sup, now);
        sup.on_event(Event::StopClicked, now + s(1));
        exited(&mut sup, 1, now + s(2));
        assert_eq!(*sup.state(), TrayState::Stopped);
    }

    #[test]
    fn start_timeout_30s_is_error() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        assert!(sup.on_event(Event::Tick, now + s(29)).is_empty());
        let a = sup.on_event(Event::Tick, now + s(30));
        assert!(matches!(sup.state(), TrayState::Error(_)));
        assert!(has(&a, &Action::KillEngineTree));
        assert!(has(&a, &Action::SetWriter(p2c_shm::Writer::Tray)));
    }

    #[test]
    fn unresponsive_while_running_is_flagged_red_until_answering() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        ready(&mut sup, now);
        assert!(!sup.is_unresponsive());
        let a = unresp(&mut sup, now + s(30));
        assert!(sup.is_unresponsive());
        assert_eq!(*sup.state(), TrayState::Running);
        assert!(a.iter().any(|x| matches!(x, Action::Notify(_))));
        // repeated reports do not re-notify
        assert!(unresp(&mut sup, now + s(32)).is_empty());
        ready(&mut sup, now + s(40));
        assert!(!sup.is_unresponsive());
    }

    #[test]
    fn force_restart_actions_order() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        ready(&mut sup, now);
        let a = sup.on_event(Event::ForceRestartClicked, now + s(5));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
        assert_eq!(a[0], Action::SetWriter(p2c_shm::Writer::Tray));
        let core: Vec<_> = a
            .iter()
            .filter(|x| {
                matches!(
                    x,
                    Action::KillEngineTree
                        | Action::StopLeftovers
                        | Action::ReRegisterCamera
                        | Action::SpawnEngine { .. }
                )
            })
            .cloned()
            .collect();
        assert_eq!(
            core,
            vec![
                Action::KillEngineTree,
                Action::StopLeftovers,
                Action::ReRegisterCamera,
                Action::SpawnEngine {
                    new_port: false,
                    gen: 2
                }
            ]
        );
        // from Stopped too
        let mut sup = Supervisor::new();
        let a = sup.on_event(Event::ForceRestartClicked, now);
        assert!(a.iter().any(is_spawn));
    }

    #[test]
    fn handover_sequence() {
        let now = t0();
        let mut sup = Supervisor::new();
        let a = sup.on_event(Event::StartClicked, now);
        let w = a
            .iter()
            .position(|x| *x == Action::SetWriter(p2c_shm::Writer::Tray));
        let sp = a.iter().position(is_spawn);
        assert!(w.unwrap() < sp.unwrap(), "{a:?}");
        // Engine is not handed the camera before it is ready.
        assert!(!a.contains(&Action::SetWriter(p2c_shm::Writer::Engine)));
        let a = ready(&mut sup, now + s(3));
        assert_eq!(a, vec![Action::SetWriter(p2c_shm::Writer::Engine)]);
        // crash: take-back then grace before standby writes
        let a = exited(&mut sup, 1, now + s(9));
        let w = a
            .iter()
            .position(|x| *x == Action::SetWriter(p2c_shm::Writer::Tray));
        let r = a
            .iter()
            .position(|x| *x == Action::ResumeStandbyAfter(Duration::from_millis(100)));
        assert!(w.unwrap() < r.unwrap(), "{a:?}");
        // Stop also takes back
        ready(&mut sup, now + s(11));
        let a = sup.on_event(Event::StopClicked, now + s(20));
        assert_eq!(a[0], Action::SetWriter(p2c_shm::Writer::Tray));
        assert!(matches!(a[1], Action::ResumeStandbyAfter(_)));
        assert!(has(&a, &Action::PostQuit));
        // Late EngineReady while stopping never re-hands the camera over.
        assert!(ready(&mut sup, now + s(21)).is_empty());
    }

    #[test]
    fn stray_events_in_stopped_are_ignored() {
        let now = t0();
        let mut sup = Supervisor::new();
        assert!(exited(&mut sup, 1, now).is_empty());
        assert!(sup.on_event(Event::StopClicked, now).is_empty());
        assert!(sup.on_event(Event::Tick, now).is_empty());
        assert!(ready(&mut sup, now).is_empty());
    }

    #[test]
    fn exits_during_sign_out_or_of_killed_engines_are_not_crashes() {
        use ExitHandling::*;
        assert_eq!(exit_handling(false, false, false), Feed);
        assert_eq!(exit_handling(true, true, false), Feed, "completes a Stop");
        assert_eq!(exit_handling(true, false, false), IgnoreKilled);
        // Sign-out wins over everything: never a restart.
        for (k, s) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(exit_handling(k, s, true), IgnoreSignOut, "{k} {s}");
        }
    }
}
