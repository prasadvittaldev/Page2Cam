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
//!   +--- exit 0 -------+ Error  <-- 76 twice, 78, start timeout
//!                      exit 75 -> Starting (restart, not counted)
//!                      exit 76 -> Starting once more on a new port
//! Force restart: any state -> Starting (kill tree, clean leftovers,
//!                re-register camera, spawn).
//! Stopping: PostQuit; after 10 s KillEngineTree; any exit -> Stopped.
//! ```
//!
//! Shell contract: deliver `EngineExited` only for the engine currently
//! tracked (not for one the shell itself killed for a Force restart), and send
//! `EngineReady` when `/api/state` first reports `output_ready`. A later
//! `EngineReady` (poll answers again) clears the "unresponsive" flag.

use std::time::{Duration, Instant};

use p2c_shm::Writer;

/// Engine start timeout (no `output_ready` by then -> Error).
pub const START_TIMEOUT: Duration = Duration::from_secs(30);
/// After `PostQuit`, wait this long before killing the engine tree.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// Two crashes within this window -> Error.
pub const CRASH_WINDOW: Duration = Duration::from_secs(120);
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
    /// later answers after an unresponsive spell).
    EngineReady,
    /// The engine process ended with this code.
    EngineExited(i32),
    /// No answer from the engine for 5 s while Running.
    EngineUnresponsive,
    /// Periodic (about 1 s) for timeouts.
    Tick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Start the engine; `new_port` = choose a fresh admin port first.
    SpawnEngine {
        new_port: bool,
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
    last_crash: Option<Instant>,
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
            last_crash: None,
            port_retried: false,
            kill_sent: false,
            unresponsive: false,
        }
    }

    pub fn state(&self) -> &TrayState {
        &self.state
    }

    /// Running but the engine has not answered for 5 s: the icon is red
    /// (state stays Running; the operator decides: Force restart).
    pub fn is_unresponsive(&self) -> bool {
        self.unresponsive
    }

    fn fresh(&mut self) {
        self.last_crash = None;
        self.port_retried = false;
        self.kill_sent = false;
        self.unresponsive = false;
    }

    fn fail(&mut self, msg: String, mut actions: Vec<Action>) -> Vec<Action> {
        self.unresponsive = false;
        actions.extend(take_back());
        actions.push(Action::Notify(msg.clone()));
        self.state = TrayState::Error(msg);
        actions
    }

    pub fn on_event(&mut self, ev: Event, now: Instant) -> Vec<Action> {
        use TrayState::*;
        match (ev, self.state.clone()) {
            (Event::StartClicked, Stopped | Error(_)) => {
                self.fresh();
                self.state = Starting { since: now };
                vec![
                    Action::SetWriter(Writer::Tray),
                    Action::SpawnEngine { new_port: false },
                ]
            }
            (Event::ForceRestartClicked, _) => {
                self.fresh();
                self.state = Starting { since: now };
                vec![
                    Action::SetWriter(Writer::Tray),
                    Action::KillEngineTree,
                    Action::StopLeftovers,
                    Action::ReRegisterCamera,
                    Action::ResumeStandbyAfter(STANDBY_GRACE),
                    Action::SpawnEngine { new_port: false },
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
            (Event::EngineReady, Starting { .. }) => {
                self.state = Running;
                vec![Action::SetWriter(Writer::Engine)]
            }
            (Event::EngineReady, Running) => {
                self.unresponsive = false;
                Vec::new()
            }
            (Event::EngineUnresponsive, Running) if !self.unresponsive => {
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
            (Event::EngineExited(_), Stopping { .. }) => {
                let mut a = take_back().to_vec();
                if self.kill_sent {
                    a.push(Action::StopLeftovers);
                }
                self.kill_sent = false;
                self.state = Stopped;
                a
            }
            (Event::EngineExited(code), Starting { .. } | Running) => self.exited(code, now),
            _ => Vec::new(),
        }
    }

    fn restart(&mut self, now: Instant, new_port: bool, mut pre: Vec<Action>) -> Vec<Action> {
        self.unresponsive = false;
        self.state = TrayState::Starting { since: now };
        pre.push(Action::SpawnEngine { new_port });
        pre
    }

    fn exited(&mut self, code: i32, now: Instant) -> Vec<Action> {
        let was_starting = matches!(self.state, TrayState::Starting { .. });
        match code {
            EXIT_RESTART => self.restart(now, false, take_back().to_vec()),
            EXIT_PORT if was_starting && !self.port_retried => {
                self.port_retried = true;
                self.restart(now, true, take_back().to_vec())
            }
            EXIT_PORT => self.fail("admin port unavailable".into(), Vec::new()),
            EXIT_CONFIG => self.fail(
                "engine misconfigured (admin token missing or invalid) - please report this".into(),
                Vec::new(),
            ),
            0 => {
                self.unresponsive = false;
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
    const SPAWN: Action = Action::SpawnEngine { new_port: false };

    #[test]
    fn start_stop_happy_path() {
        let now = t0();
        let mut sup = Supervisor::new();
        assert_eq!(*sup.state(), TrayState::Stopped);
        let a = sup.on_event(Event::StartClicked, now);
        assert_eq!(*sup.state(), TrayState::Starting { since: now });
        assert!(has(&a, &SPAWN));
        let a = sup.on_event(Event::EngineReady, now + s(2));
        assert_eq!(*sup.state(), TrayState::Running);
        assert_eq!(a, vec![Action::SetWriter(p2c_shm::Writer::Engine)]);
        let a = sup.on_event(Event::StopClicked, now + s(60));
        assert_eq!(*sup.state(), TrayState::Stopping { since: now + s(60) });
        assert!(has(&a, &Action::PostQuit));
        sup.on_event(Event::EngineExited(0), now + s(62));
        assert_eq!(*sup.state(), TrayState::Stopped);
    }

    #[test]
    fn stop_timeout_kills_tree() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        sup.on_event(Event::EngineReady, now);
        sup.on_event(Event::StopClicked, now + s(1));
        assert!(sup.on_event(Event::Tick, now + s(10)).is_empty());
        let a = sup.on_event(Event::Tick, now + s(11));
        assert_eq!(a, vec![Action::KillEngineTree]);
        // only once
        assert!(sup.on_event(Event::Tick, now + s(12)).is_empty());
        let a = sup.on_event(Event::EngineExited(1), now + s(12));
        assert_eq!(*sup.state(), TrayState::Stopped);
        assert!(has(&a, &Action::StopLeftovers));
        assert!(has(&a, &Action::SetWriter(p2c_shm::Writer::Tray)));
    }

    #[test]
    fn crash_restarts_once_then_error() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        sup.on_event(Event::EngineReady, now);
        let a = sup.on_event(Event::EngineExited(1), now + s(10));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
        assert!(has(&a, &SPAWN));
        assert!(a.iter().any(|x| matches!(x, Action::Notify(_))));
        sup.on_event(Event::EngineReady, now + s(12));
        let a = sup.on_event(Event::EngineExited(0xC0000005u32 as i32), now + s(60));
        assert!(matches!(sup.state(), TrayState::Error(_)));
        assert!(!a.iter().any(|x| matches!(x, Action::SpawnEngine { .. })));
        assert!(has(&a, &Action::SetWriter(p2c_shm::Writer::Tray)));
        // Start clicked in Error starts fresh.
        let a = sup.on_event(Event::StartClicked, now + s(70));
        assert!(has(&a, &SPAWN));
    }

    #[test]
    fn crash_after_two_minutes_is_a_fresh_first_crash() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        sup.on_event(Event::EngineReady, now);
        sup.on_event(Event::EngineExited(1), now + s(10));
        sup.on_event(Event::EngineReady, now + s(12));
        sup.on_event(Event::EngineExited(1), now + s(10 + 121));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
    }

    #[test]
    fn exit_75_restarts_without_counting() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        sup.on_event(Event::EngineReady, now);
        for i in 0..3 {
            let a = sup.on_event(Event::EngineExited(75), now + s(10 + i));
            assert!(has(&a, &SPAWN));
            assert!(matches!(sup.state(), TrayState::Starting { .. }));
            sup.on_event(Event::EngineReady, now + s(10 + i));
        }
        // A later crash is still the first one: restarted, not Error.
        sup.on_event(Event::EngineExited(1), now + s(20));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
    }

    #[test]
    fn exit_76_retries_new_port_once() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        let a = sup.on_event(Event::EngineExited(76), now + s(1));
        assert!(has(&a, &Action::SpawnEngine { new_port: true }));
        assert!(matches!(sup.state(), TrayState::Starting { .. }));
        let a = sup.on_event(Event::EngineExited(76), now + s(2));
        assert!(matches!(sup.state(), TrayState::Error(_)));
        assert!(!a.iter().any(|x| matches!(x, Action::SpawnEngine { .. })));
    }

    #[test]
    fn exit_78_is_error_without_restart() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        let a = sup.on_event(Event::EngineExited(78), now + s(1));
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
        sup.on_event(Event::EngineReady, now);
        let a = sup.on_event(Event::EngineExited(0), now + s(5));
        assert_eq!(*sup.state(), TrayState::Stopped);
        assert!(!a.iter().any(|x| matches!(x, Action::SpawnEngine { .. })));
    }

    #[test]
    fn exit_while_stopping_is_always_normal() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        sup.on_event(Event::EngineReady, now);
        sup.on_event(Event::StopClicked, now + s(1));
        sup.on_event(Event::EngineExited(1), now + s(2));
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
        sup.on_event(Event::EngineReady, now);
        assert!(!sup.is_unresponsive());
        let a = sup.on_event(Event::EngineUnresponsive, now + s(30));
        assert!(sup.is_unresponsive());
        assert_eq!(*sup.state(), TrayState::Running);
        assert!(a.iter().any(|x| matches!(x, Action::Notify(_))));
        // repeated reports do not re-notify
        assert!(sup
            .on_event(Event::EngineUnresponsive, now + s(32))
            .is_empty());
        sup.on_event(Event::EngineReady, now + s(40));
        assert!(!sup.is_unresponsive());
    }

    #[test]
    fn force_restart_actions_order() {
        let now = t0();
        let mut sup = Supervisor::new();
        started(&mut sup, now);
        sup.on_event(Event::EngineReady, now);
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
                SPAWN
            ]
        );
        // from Stopped too
        let mut sup = Supervisor::new();
        let a = sup.on_event(Event::ForceRestartClicked, now);
        assert!(has(&a, &SPAWN));
    }

    #[test]
    fn handover_sequence() {
        let now = t0();
        let mut sup = Supervisor::new();
        let a = sup.on_event(Event::StartClicked, now);
        let w = a
            .iter()
            .position(|x| *x == Action::SetWriter(p2c_shm::Writer::Tray));
        let sp = a.iter().position(|x| *x == SPAWN);
        assert!(w.unwrap() < sp.unwrap(), "{a:?}");
        // Engine is not handed the camera before it is ready.
        assert!(!a.contains(&Action::SetWriter(p2c_shm::Writer::Engine)));
        let a = sup.on_event(Event::EngineReady, now + s(3));
        assert_eq!(a, vec![Action::SetWriter(p2c_shm::Writer::Engine)]);
        // crash: take-back then grace before standby writes
        let a = sup.on_event(Event::EngineExited(1), now + s(9));
        let w = a
            .iter()
            .position(|x| *x == Action::SetWriter(p2c_shm::Writer::Tray));
        let r = a
            .iter()
            .position(|x| *x == Action::ResumeStandbyAfter(Duration::from_millis(100)));
        assert!(w.unwrap() < r.unwrap(), "{a:?}");
        // Stop also takes back
        sup.on_event(Event::EngineReady, now + s(11));
        let a = sup.on_event(Event::StopClicked, now + s(20));
        assert_eq!(a[0], Action::SetWriter(p2c_shm::Writer::Tray));
        assert!(matches!(a[1], Action::ResumeStandbyAfter(_)));
        assert!(has(&a, &Action::PostQuit));
        // Late EngineReady while stopping never re-hands the camera over.
        assert!(sup.on_event(Event::EngineReady, now + s(21)).is_empty());
    }

    #[test]
    fn stray_events_in_stopped_are_ignored() {
        let now = t0();
        let mut sup = Supervisor::new();
        assert!(sup.on_event(Event::EngineExited(1), now).is_empty());
        assert!(sup.on_event(Event::StopClicked, now).is_empty());
        assert!(sup.on_event(Event::Tick, now).is_empty());
        assert!(sup.on_event(Event::EngineReady, now).is_empty());
    }
}
