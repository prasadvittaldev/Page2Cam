//! Engine mode: Page2Cam run by the tray (`--engine`).
//!
//! The tray owns the virtual camera, the update checks and restarts; the
//! engine renders, writes frames (only while the tray hands it the camera),
//! serves the admin page on the port/token the tray gave it, reports health,
//! and exits with a code the tray understands. It also exits when the tray
//! (`--parent-pid`) is gone, so it never outlives it.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Normal exit (quit route, Ctrl-C, parent gone).
pub const EXIT_OK: i32 = 0;
/// Something else ended the engine (Chromium closed by itself, start failed).
pub const EXIT_CRASH: i32 = 1;
/// "Restart now" / GPU change: the tray starts the engine again (not a crash).
pub const EXIT_RESTART: i32 = 75;
/// `--admin-port` could not be bound: the tray retries with another port.
pub const EXIT_PORT: i32 = 76;

/// Env var the tray passes the admin token in (never on the command line).
pub const TOKEN_ENV: &str = "P2C_ADMIN_TOKEN";

/// How often the parent watchdog looks at the tray.
pub const WATCH_EVERY: Duration = Duration::from_secs(1);

/// Which Page2Cam this process is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Today's app: own camera registration, console, self-relaunch, updates.
    Standalone,
    /// Run by the tray (`--engine`).
    Engine,
}

impl Role {
    /// Windows Error Reporting restarts us after a crash (standalone only;
    /// the tray supervises the engine).
    pub fn register_crash_restart(self) -> bool {
        self == Role::Standalone
    }
    /// R28 relaunch after Chromium closed, and "Restart now" as a relaunch
    /// (the engine exits with a code instead).
    pub fn self_relaunch(self) -> bool {
        self == Role::Standalone
    }
    /// Update checks (the tray runs them in engine mode).
    pub fn check_updates(self, setting: bool) -> bool {
        self == Role::Standalone && setting
    }
    /// The "Control it here: <url>" line on stdout.
    pub fn print_admin_url(self) -> bool {
        self == Role::Standalone
    }
    /// Open the admin page in the default browser at start.
    pub fn open_browser(self, setting: bool, no_open: bool) -> bool {
        self == Role::Standalone && setting && !no_open
    }
    /// Bind exactly the given admin port (no fallback; exit 76 instead).
    pub fn strict_port(self) -> bool {
        self == Role::Engine
    }
}

/// The engine's exit code: restart requested → 75; a normal shutdown that
/// went fine → 0; anything else → 1 (the tray counts it as a crash).
pub fn exit_code(restart_requested: bool, user_shutdown: bool, ok: bool) -> i32 {
    if restart_requested {
        EXIT_RESTART
    } else if user_shutdown && ok {
        EXIT_OK
    } else {
        EXIT_CRASH
    }
}

/// The admin token to use: in engine mode the tray's (from [`TOKEN_ENV`]) if
/// it is usable in a URL query (16..=128 of `[A-Za-z0-9_-]`); else `None`
/// (a generated token, as standalone always does).
pub fn admin_token(role: Role, env: Option<String>) -> Option<String> {
    if role != Role::Engine {
        return None;
    }
    let t = env?;
    let ok = (16..=128).contains(&t.len())
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !ok {
        tracing::warn!("{TOKEN_ENV} is not a usable token; using a generated one");
        return None;
    }
    Some(t)
}

/// Check `alive` every `every` until it says no (→ true) or `stop` is set
/// (→ false: we are closing anyway).
pub fn watch_parent(mut alive: impl FnMut() -> bool, every: Duration, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        if !alive() {
            return true;
        }
        std::thread::sleep(every);
    }
}

/// Watch `parent` on a thread (every [`WATCH_EVERY`]); when it is gone call
/// `on_gone` once (the normal shutdown). Ends quietly once `quit` is set.
pub fn spawn_parent_watchdog(
    parent: Parent,
    quit: Arc<AtomicBool>,
    on_gone: Box<dyn FnOnce() + Send>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("p2c-parent-watch".into())
        .spawn(move || {
            if watch_parent(|| parent.alive(), WATCH_EVERY, &quit) {
                tracing::warn!(pid = parent.pid(), "the tray is gone; shutting down");
                on_gone();
            }
        })?;
    Ok(())
}

/// The tray process, held open from start so a reused pid can't fool us.
#[cfg(windows)]
pub struct Parent {
    pid: u32,
    handle: windows::Win32::Foundation::HANDLE,
}

// SAFETY: a process handle may be waited on from any thread; we only wait and close.
#[cfg(windows)]
unsafe impl Send for Parent {}

#[cfg(windows)]
impl Parent {
    /// `None` if `pid` is not running (or can't be opened).
    pub fn open(pid: u32) -> Option<Parent> {
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
        // SAFETY: plain Win32 call; the handle is closed in Drop.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.ok()?;
        Some(Parent { pid, handle })
    }

    pub fn alive(&self) -> bool {
        use windows::Win32::Foundation::WAIT_TIMEOUT;
        use windows::Win32::System::Threading::WaitForSingleObject;
        // SAFETY: the handle is valid until Drop.
        unsafe { WaitForSingleObject(self.handle, 0) == WAIT_TIMEOUT }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

#[cfg(windows)]
impl Drop for Parent {
    fn drop(&mut self) {
        // SAFETY: opened in `open`, closed once.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

/// The tray process (Linux: by pid and start time, so a reused pid reads as gone).
#[cfg(not(windows))]
pub struct Parent {
    pid: u32,
    started: String,
}

#[cfg(not(windows))]
impl Parent {
    /// Start time field of `/proc/<pid>/stat`, unless the process is a zombie.
    fn start_time(pid: u32) -> Option<String> {
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (_, rest) = s.rsplit_once(')')?;
        let f: Vec<&str> = rest.split_whitespace().collect();
        if f.first() == Some(&"Z") {
            return None;
        }
        f.get(19).map(|s| s.to_string())
    }

    pub fn open(pid: u32) -> Option<Parent> {
        Some(Parent {
            pid,
            started: Self::start_time(pid)?,
        })
    }

    pub fn alive(&self) -> bool {
        Self::start_time(self.pid).as_ref() == Some(&self.started)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::time::Instant;

    #[test]
    fn restart_in_engine_mode_exits_75() {
        // "Restart now" / GPU change: the tray starts us again.
        assert_eq!(exit_code(true, false, true), 75);
        assert_eq!(exit_code(true, true, true), 75);
        assert_eq!(exit_code(true, false, false), 75);
        assert_eq!(EXIT_RESTART, 75);
        // Quit route, Ctrl-C, parent gone: a normal exit.
        assert_eq!(exit_code(false, true, true), 0);
        // The page engine closed by itself, or startup failed: a crash.
        assert_eq!(exit_code(false, false, true), 1);
        assert_eq!(exit_code(false, true, false), 1);
        assert_eq!(EXIT_PORT, 76);
    }

    #[test]
    fn engine_mode_disables_standalone_extras() {
        let e = Role::Engine;
        assert!(!e.register_crash_restart());
        assert!(!e.self_relaunch());
        assert!(!e.check_updates(true));
        assert!(!e.print_admin_url());
        assert!(!e.open_browser(true, false));
        assert!(e.strict_port());
        let s = Role::Standalone;
        assert!(s.register_crash_restart());
        assert!(s.self_relaunch());
        assert!(s.check_updates(true));
        assert!(!s.check_updates(false));
        assert!(s.print_admin_url());
        assert!(s.open_browser(true, false));
        assert!(!s.open_browser(true, true));
        assert!(!s.open_browser(false, false));
        assert!(!s.strict_port());
    }

    #[test]
    fn admin_token_only_from_env_in_engine_mode() {
        let good = "0123456789abcdef0123456789abcdef".to_string();
        assert_eq!(
            admin_token(Role::Engine, Some(good.clone())),
            Some(good.clone())
        );
        // Standalone ignores it (a generated token, as before).
        assert_eq!(admin_token(Role::Standalone, Some(good)), None);
        assert_eq!(admin_token(Role::Engine, None), None);
        // Unusable tokens are refused (too short, not URL-safe).
        assert_eq!(admin_token(Role::Engine, Some("abc".into())), None);
        assert_eq!(
            admin_token(Role::Engine, Some("0123456789abcdef&t=x0123456789".into())),
            None
        );
    }

    #[test]
    fn exits_when_parent_disappears() {
        // Pure loop: alive for three checks, then gone -> shutdown.
        let calls = AtomicU32::new(0);
        let stop = AtomicBool::new(false);
        let gone = watch_parent(
            || calls.fetch_add(1, Ordering::SeqCst) < 3,
            Duration::from_millis(5),
            &stop,
        );
        assert!(gone);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        // Stopped (normal shutdown already running): no second shutdown.
        let stop = AtomicBool::new(true);
        assert!(!watch_parent(|| true, Duration::from_millis(5), &stop));

        // A real process: the watchdog fires within ~2 s of its death.
        let mut child = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
                .spawn()
        } else {
            std::process::Command::new("sleep").arg("30").spawn()
        }
        .unwrap();
        let parent = Parent::open(child.id()).expect("child is running");
        assert!(parent.alive());
        let (tx, rx) = std::sync::mpsc::channel();
        let quit = Arc::new(AtomicBool::new(false));
        spawn_parent_watchdog(
            parent,
            quit.clone(),
            Box::new(move || tx.send(Instant::now()).unwrap()),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(rx.try_recv().is_err(), "parent still alive");
        let killed = Instant::now();
        child.kill().unwrap();
        let _ = child.wait();
        let fired = rx
            .recv_timeout(Duration::from_secs(3))
            .expect("watchdog did not fire");
        assert!(fired.duration_since(killed) <= Duration::from_millis(2100));
        quit.store(true, Ordering::SeqCst);
    }

    #[test]
    fn a_gone_parent_cannot_be_opened() {
        let mut child = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "exit"])
                .spawn()
        } else {
            std::process::Command::new("true").spawn()
        }
        .unwrap();
        let pid = child.id();
        let _ = child.wait();
        assert!(Parent::open(pid).is_none_or(|p| !p.alive()));
    }
}
