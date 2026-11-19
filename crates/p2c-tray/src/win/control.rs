//! The tray's controller thread: owns the [`Supervisor`], the engine child,
//! the camera and the health state; carries out the supervisor's actions;
//! tells the UI thread what to show. Never blocks the UI (which only sends
//! commands and reads [`UiState`]).
//!
//! Wakes once a second (timeouts, standby frame, health) and on commands;
//! no busy loops. The engine is polled every 2 s only while it runs.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use p2c_admin::Health;
use parking_lot::Mutex;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL};

use super::camera::Camera;
use super::engine::{self, Engine};
use super::sysload::Sampler;
use crate::health::{classify, display, Hysteresis, Level, SysLoad};
use crate::menu::{Entry, MenuId, UpdateOffer};
use crate::standby::StandbyKind;
use crate::state::{Action, Event, Supervisor, TrayState};
use crate::update::Updater;

/// Commands for the controller (from the UI and the engine threads).
pub enum Cmd {
    Menu(MenuId),
    /// A second launch, or the user wants to see Page2Cam.
    Show,
    EngineExited {
        gen: u64,
        code: i32,
    },
    Poll {
        gen: u64,
        result: Result<Health, String>,
    },
    /// A message from a worker thread (shown as a balloon).
    Notify(String),
    /// Windows is ending the session: stop the engine gracefully (short
    /// bounded wait), remove the camera, then signal the sender.
    EndSession(Sender<()>),
}

pub use crate::notify::Balloon;

/// What the UI thread shows; written by the controller, read on `WM_REFRESH`.
pub struct UiState {
    pub level: Level,
    pub tooltip: String,
    pub entries: Vec<Entry>,
    pub balloons: Vec<Balloon>,
    pub show_menu: bool,
    pub quit: bool,
}

/// Shared between the controller and the UI thread.
pub struct Ui {
    pub state: Mutex<UiState>,
    /// The tray window (an HWND; stored as a number to cross threads).
    pub hwnd: isize,
    pub refresh_msg: u32,
}

impl Ui {
    pub fn new(hwnd: isize, refresh_msg: u32) -> Ui {
        Ui {
            state: Mutex::new(UiState {
                level: Level::Grey,
                tooltip: "Page2Cam — starting the tray…".into(),
                entries: Vec::new(),
                balloons: Vec::new(),
                show_menu: false,
                quit: false,
            }),
            hwnd,
            refresh_msg,
        }
    }

    /// Ask the UI thread to look at `state`.
    pub fn poke(&self) {
        // SAFETY: posting to our own window; a stale handle just fails.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.hwnd as *mut _)),
                self.refresh_msg,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }

    pub fn balloon(&self, text: impl Into<String>, warning: bool) {
        self.state.lock().balloons.push(Balloon {
            text: text.into(),
            warning,
        });
        self.poke();
    }
}

const TICK: Duration = Duration::from_secs(1);
/// No answer for this long while Running: unresponsive (red).
const UNRESPONSIVE_AFTER: Duration = Duration::from_secs(5);
/// Quit gives the engine this long in total, then ends it.
const QUIT_LIMIT: Duration = Duration::from_secs(15);
/// At sign-out Windows waits only a few seconds for us.
const END_SESSION_LIMIT: Duration = Duration::from_secs(2);
const SYSLOAD_EVERY: Duration = Duration::from_secs(2);
const FIRST_RUN: &str = "Page2Cam is in your tray. Click Start when you need it.";

struct Controller {
    sup: Supervisor,
    hy: Hysteresis,
    reasons: Vec<String>,
    /// Declared before `cam`: dropped (killed) first.
    engine: Option<Engine>,
    /// Generations the tray killed itself: their exit is never a crash.
    killed: Vec<u64>,
    port: Option<u16>,
    token: String,
    last_answer: Option<Instant>,
    cam: Camera,
    sampler: Option<Sampler>,
    sys: SysLoad,
    next_sys: Instant,
    updater: Arc<Updater>,
    offered: Option<String>,
    autostart: bool,
    exe: PathBuf,
    tx: Sender<Cmd>,
    ui: Arc<Ui>,
    quitting: Option<Instant>,
    /// How long Quit waits for the engine (shorter at sign-out).
    quit_limit: Duration,
    /// Sign-out: told when everything is closed.
    end_session: Option<Sender<()>>,
    done: bool,
    level: Level,
}

fn random_token() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..32)
        .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
        .collect()
}

/// The controller thread's body. Returns when the tray quits.
pub fn run(rx: Receiver<Cmd>, tx: Sender<Cmd>, ui: Arc<Ui>) {
    let me = std::process::id();
    let exe = std::env::current_exe().unwrap_or_default();
    let mut c = Controller {
        sup: Supervisor::new(),
        hy: Hysteresis::new(),
        reasons: Vec::new(),
        engine: None,
        killed: Vec::new(),
        port: None,
        token: random_token(),
        last_answer: None,
        cam: Camera::closed(),
        sampler: None,
        sys: SysLoad::default(),
        next_sys: Instant::now(),
        updater: super::update::start(),
        offered: None,
        autostart: super::autostart::enabled(&exe),
        exe,
        tx,
        ui,
        quitting: None,
        quit_limit: QUIT_LIMIT,
        end_session: None,
        done: false,
        level: Level::Grey,
    };
    // Menu first, so the tray is usable while the rest starts.
    c.refresh_ui();
    // Leftovers (old engines, a standalone Page2Cam, orphaned helpers) would
    // hold the camera: stop them before registering it. Only the tray and
    // its ancestors are spared.
    let stopped = p2c_proc::stop_others_for_tray(me);
    if !stopped.is_empty() {
        tracing::info!(?stopped, "stopped leftover Page2Cam processes");
    }
    // Registers in the background: a wedged Frame Server can't block the
    // menu, Start/Stop or Quit.
    c.cam = Camera::open();
    if !crate::settings::tray_seen() {
        c.ui.balloon(FIRST_RUN, false);
        if let Err(e) = crate::settings::set_tray_seen() {
            tracing::warn!("cannot save tray.toml: {e:#}");
        }
    }
    c.refresh_ui();
    let mut next_tick = Instant::now() + TICK;
    while !c.done {
        let now = Instant::now();
        let kind = if c.level == Level::Red {
            StandbyKind::Attention
        } else {
            StandbyKind::Stopped
        };
        let wake = c.cam.service(now, kind).min(next_tick);
        match rx.recv_timeout(wake.saturating_duration_since(now)) {
            Ok(cmd) => c.handle(cmd, Instant::now()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let now = Instant::now();
        if now >= next_tick {
            c.tick(now);
            next_tick = now + TICK;
        }
    }
    c.finish();
}

impl Controller {
    fn running(&self) -> bool {
        matches!(self.sup.state(), TrayState::Running)
    }

    fn handle(&mut self, cmd: Cmd, now: Instant) {
        match cmd {
            Cmd::Menu(id) => self.menu(id, now),
            Cmd::Show => {
                if self.running() {
                    self.open_page();
                } else {
                    self.ui.state.lock().show_menu = true;
                    self.ui.poke();
                }
            }
            Cmd::EngineExited { gen, code } => {
                if self.engine.as_ref().is_some_and(|e| e.gen == gen) {
                    // Closing its job also ends helpers it left behind.
                    self.engine = None;
                }
                let killed = self.killed.contains(&gen);
                self.killed.retain(|g| *g != gen);
                // An engine we killed ourselves: its exit only completes a
                // Stop; it never counts as a crash.
                if killed && !matches!(self.sup.state(), TrayState::Stopping { .. }) {
                    tracing::info!(gen, code, "exit of an engine the tray ended; ignored");
                } else {
                    self.feed(Event::EngineExited { gen, code }, now);
                }
            }
            Cmd::Poll { gen, result } => self.poll_result(gen, result, now),
            Cmd::Notify(text) => self.ui.balloon(text, true),
            Cmd::EndSession(done) => {
                tracing::info!("Windows is ending the session; closing");
                self.end_session = Some(done);
                self.quit_limit = END_SESSION_LIMIT;
                self.begin_quit(now);
            }
        }
        self.refresh_ui();
    }

    fn menu(&mut self, id: MenuId, now: Instant) {
        tracing::info!(?id, "tray menu");
        // Quit is final: nothing else is carried out once it was chosen.
        if self.quitting.is_some() && id != MenuId::Quit {
            tracing::info!(?id, "ignored while quitting");
            return;
        }
        match id {
            MenuId::Start => self.feed(Event::StartClicked, now),
            MenuId::Stop => self.feed(Event::StopClicked, now),
            MenuId::ForceRestart => self.feed(Event::ForceRestartClicked, now),
            MenuId::OpenPage => self.open_page(),
            MenuId::Update => self.update(),
            MenuId::Autostart => {
                let on = !super::autostart::enabled(&self.exe);
                match super::autostart::set_enabled(&self.exe, on) {
                    Ok(()) => {
                        tracing::info!(on, "start with Windows");
                        self.autostart = on;
                    }
                    Err(e) => self
                        .ui
                        .balloon(format!("Could not change Start with Windows: {e:#}"), true),
                }
            }
            MenuId::Quit => self.begin_quit(now),
        }
    }

    /// Stop the engine (bounded by `quit_limit`), then `finish`.
    fn begin_quit(&mut self, now: Instant) {
        if self.quitting.is_none() {
            tracing::info!("quitting");
            self.quitting = Some(now);
            self.feed(Event::StopClicked, now);
        }
        self.check_quit(now);
    }

    fn poll_result(&mut self, gen: u64, result: Result<Health, String>, now: Instant) {
        if gen != self.sup.generation() || self.engine.as_ref().is_none_or(|e| e.gen != gen) {
            return;
        }
        match result {
            Ok(mut h) => {
                self.last_answer = Some(now);
                if h.output_ready {
                    self.feed(Event::EngineReady { gen }, now);
                }
                if self.running() {
                    // The tray owns the camera: its status is the truth.
                    h.camera_status = self.cam.status_line();
                    let (raw, why) = classify(&h, &self.sys);
                    let (_, reasons) = self.hy.update(raw, why, now);
                    self.reasons = reasons;
                }
            }
            Err(e) => {
                tracing::debug!(gen, "engine poll failed: {e}");
                self.hy.no_sample();
            }
        }
    }

    fn feed(&mut self, ev: Event, now: Instant) {
        let before = self.sup.state().clone();
        let actions = self.sup.on_event(ev, now);
        let after = self.sup.state().clone();
        if before != after {
            tracing::info!(from = ?before, to = ?after, "tray state");
            if matches!(after, TrayState::Running) {
                self.hy.reset();
                self.reasons.clear();
                self.last_answer = Some(now);
            }
        }
        for a in actions {
            self.exec(a, now);
        }
    }

    fn exec(&mut self, a: Action, now: Instant) {
        tracing::debug!(?a, "tray action");
        match a {
            Action::SpawnEngine { new_port, gen } => self.spawn(new_port, gen),
            Action::PostQuit => {
                if let Some(port) = self.engine.as_ref().map(|e| e.port) {
                    let token = self.token.clone();
                    let _ = std::thread::Builder::new()
                        .name("p2c-tray-quit".into())
                        .spawn(move || {
                            if let Err(e) = crate::http::post_quit(port, &token) {
                                tracing::warn!("asking the engine to stop: {e:#}");
                            }
                        });
                }
            }
            Action::KillEngineTree => {
                if let Some(e) = self.engine.take() {
                    self.killed.push(e.gen);
                    e.kill();
                }
            }
            Action::StopLeftovers => {
                p2c_proc::stop_others(std::process::id(), None, &[]);
            }
            Action::ReRegisterCamera => self.cam.re_register(),
            Action::SetWriter(w) => self.cam.set_writer(w),
            Action::ResumeStandbyAfter(d) => self.cam.resume_at(now + d),
            Action::Notify(text) => {
                let warning = matches!(self.sup.state(), TrayState::Error(_));
                self.ui.balloon(text, warning);
            }
        }
    }

    fn spawn(&mut self, new_port: bool, gen: u64) {
        if let Some(old) = self.engine.take() {
            // Not expected (the supervisor ends an engine before the next).
            tracing::warn!(gen = old.gen, "ending an engine that was still tracked");
            self.killed.push(old.gen);
            old.kill();
        }
        self.last_answer = None;
        self.hy.reset();
        self.reasons.clear();
        if new_port || self.port.is_none() {
            self.port = crate::launch::pick_free_port().ok();
        }
        let result = match self.port {
            Some(port) => engine::spawn(gen, port, &self.token, self.tx.clone()),
            None => Err(anyhow::anyhow!("no free port for the control page")),
        };
        match result {
            Ok(e) => self.engine = Some(e),
            Err(e) => {
                tracing::error!("cannot start the engine: {e:#}");
                self.ui
                    .balloon(format!("Page2Cam could not start: {e:#}"), true);
                // Counts like a crash (restart once, then red).
                let _ = self.tx.send(Cmd::EngineExited { gen, code: 1 });
            }
        }
    }

    fn tick(&mut self, now: Instant) {
        self.feed(Event::Tick, now);
        if self.running()
            && self.engine.is_some()
            && self
                .last_answer
                .is_some_and(|t| now.saturating_duration_since(t) >= UNRESPONSIVE_AFTER)
        {
            let gen = self.sup.generation();
            self.feed(Event::EngineUnresponsive { gen }, now);
        }
        let started = matches!(
            self.sup.state(),
            TrayState::Starting { .. } | TrayState::Running
        );
        if started {
            if now >= self.next_sys {
                self.next_sys = now + SYSLOAD_EVERY;
                if self.sampler.is_none() {
                    self.sampler = Sampler::new();
                }
                if let Some(s) = &mut self.sampler {
                    self.sys = s.sample();
                }
            }
        } else if self.sampler.is_some() {
            self.sampler = None;
            self.sys = SysLoad::default();
        }
        self.check_quit(now);
        self.refresh_ui();
    }

    fn check_quit(&mut self, now: Instant) {
        let Some(since) = self.quitting else {
            return;
        };
        let idle = matches!(self.sup.state(), TrayState::Stopped | TrayState::Error(_))
            && self.engine.is_none();
        if idle || now.saturating_duration_since(since) >= self.quit_limit {
            self.done = true;
        }
    }

    /// End everything: engine tree, camera registration; tell the UI.
    fn finish(&mut self) {
        if let Some(e) = self.engine.take() {
            e.kill();
        }
        // Removing the camera waits for Frame Server; don't let a hung
        // service keep the tray from closing.
        let cam = std::mem::replace(&mut self.cam, Camera::closed());
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let spawned = std::thread::Builder::new()
            .name("p2c-tray-cam-remove".into())
            .spawn(move || {
                drop(cam);
                drop(tx);
            });
        if spawned.is_ok()
            && rx
                .recv_timeout(Duration::from_secs(3))
                .is_err_and(|e| e == RecvTimeoutError::Timeout)
        {
            tracing::warn!("removing the virtual camera hangs; closing anyway");
        }
        let mut s = self.ui.state.lock();
        s.quit = true;
        drop(s);
        self.ui.poke();
        tracing::info!("tray stopped");
        // Sign-out: everything is closed; let the window thread return.
        drop(self.end_session.take());
    }

    fn open_page(&self) {
        let Some(port) = self.engine.as_ref().map(|e| e.port) else {
            return;
        };
        let url = format!("http://127.0.0.1:{port}/?t={}", self.token);
        open_url(&url);
    }

    fn update(&mut self) {
        let info = self.updater.info();
        if info.state != "available" {
            return;
        }
        if !info.can_install {
            open_url(&info.page_url);
            return;
        }
        let updater = self.updater.clone();
        let tx = self.tx.clone();
        self.ui
            .balloon(format!("Downloading Page2Cam {}…", info.version), false);
        let _ = std::thread::Builder::new()
            .name("p2c-tray-install".into())
            .spawn(move || match updater.install() {
                // The installer stops and restarts Page2Cam; close cleanly first.
                Ok(()) => {
                    let _ = tx.send(Cmd::Menu(MenuId::Quit));
                }
                Err(e) => {
                    let _ = tx.send(Cmd::Notify(format!("Update failed: {e:#}")));
                }
            });
    }

    fn refresh_ui(&mut self) {
        let state = self.sup.state().clone();
        let problem = self.cam.problem();
        let (level, tip) = display(
            &state,
            self.sup.is_unresponsive(),
            self.hy.level(),
            &self.reasons,
            problem.as_deref(),
        );
        self.level = level;
        let info = self.updater.info();
        let offer = (info.state == "available").then(|| UpdateOffer {
            version: info.version.clone(),
            can_install: info.can_install,
        });
        if offer.as_ref().map(|o| &o.version) != self.offered.as_ref() {
            self.offered = offer.as_ref().map(|o| o.version.clone());
            if let Some(o) = &offer {
                self.ui.balloon(
                    format!(
                        "Page2Cam {} is available. Install it from the tray menu.",
                        o.version
                    ),
                    false,
                );
            }
        }
        let entries = if self.quitting.is_some() {
            crate::menu::quitting(&tip)
        } else {
            crate::menu::entries(&state, &tip, offer.as_ref(), self.autostart)
        };
        let mut s = self.ui.state.lock();
        if s.level != level || s.tooltip != tip || s.entries != entries {
            s.level = level;
            s.tooltip = tip;
            s.entries = entries;
            drop(s);
            self.ui.poke();
        }
    }
}

/// Open `url` in the default browser. ShellExecute wants COM initialised
/// (STA): done on a short-lived thread of its own, never the controller.
fn open_url(url: &str) {
    let url = url.to_string();
    let spawned = std::thread::Builder::new()
        .name("p2c-tray-open".into())
        .spawn(move || {
            use windows::Win32::System::Com::{
                CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
            };
            // SAFETY: COM init/uninit paired on this thread; valid strings.
            unsafe {
                let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
                let r = ShellExecuteW(
                    None,
                    &HSTRING::from("open"),
                    &HSTRING::from(url.as_str()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                );
                // ShellExecute returns a value > 32 on success.
                if (r.0 as isize) <= 32 {
                    tracing::warn!(code = r.0 as isize, "could not open the browser");
                }
                if com.is_ok() {
                    CoUninitialize();
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("could not open the browser: {e}");
    }
}
