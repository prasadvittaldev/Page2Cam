//! Page2Cam process housekeeping, shared by the engine/standalone app and
//! the tray: stop leftover Page2Cam processes, and named locks
//! (`Local\Page2Cam.Engine` for the engine, `Local\Page2Cam.Tray` for the
//! tray).
//!
//! # Stop leftover Page2Cam processes at start
//!
//! A previous instance (or its orphaned Chromium helpers) can keep the
//! virtual camera, the admin port or the CEF profile locked. Before opening
//! any of them we stop every other Page2Cam process, except ourselves, our
//! own helpers (descendants) and our ancestors (e.g. a tray process that
//! started the engine).
//!
//! Two launches milliseconds apart must not stop each other: an instance
//! started just after us (within `RACE_WINDOW`) is spared, and afterwards
//! the engine lock (`Local\Page2Cam.Engine` on Windows, a flock'd file on
//! Linux) lets only one of them run.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// One running process, as seen by the platform scan.
#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
    pub path: Option<PathBuf>,
    /// Start time in platform ticks (`platform::TICKS_PER_SEC`), when known.
    pub started: Option<u64>,
    /// A `page2cam --mcp` bridge (a desktop agent's connection): never
    /// stopped as a leftover.
    pub mcp_bridge: bool,
}

/// Prefix of the lock each `--mcp` bridge holds (`<prefix><pid>`), so the
/// process sweeps can tell it apart without reading its command line.
pub const MCP_LOCK_PREFIX: &str = "Page2Cam.Mcp.";

/// The lock name a bridge with this pid holds.
pub fn mcp_lock_name(pid: u32) -> String {
    format!("{MCP_LOCK_PREFIX}{pid}")
}

/// `page2cam --mcp` and nothing else: the MCP bridge role.
pub fn is_mcp_args<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> bool {
    args.len() == 2 && args[1].as_ref() == "--mcp"
}

/// Maximum parent-chain steps followed (guards against pid-reuse cycles).
const MAX_CHAIN: usize = 64;

const WAIT_EXIT: Duration = Duration::from_secs(3);

/// An instance started at most this long after us is a racing launch and is
/// left alone (the engine lock settles it). Started even later than that is a
/// clock jump (Windows start times are wall-clock), so it is stopped as usual.
const RACE_WINDOW: u64 = 10 * platform::TICKS_PER_SEC;

/// How long to wait for another instance's engine lock before stopping the
/// other instances once more.
const LOCK_WAIT: Duration = Duration::from_secs(3);

/// Name of the engine lock. Only the engine (or standalone app) takes it.
const ENGINE_LOCK: &str = "Page2Cam.Engine";

/// Name of the tray's single-instance lock (`Local\Page2Cam.Tray` on
/// Windows). Only the tray takes it; `--standalone` refuses to start while
/// it is held.
pub const TRAY_LOCK: &str = "Page2Cam.Tray";

pub use platform::Lock;

/// Take the named lock `name` (`Local\<name>` on Windows, a flock'd file on
/// Linux). `Ok(None)` when another process (or handle) already holds it.
pub fn try_lock(name: &str) -> anyhow::Result<Option<Lock>> {
    platform::try_lock(name)
}

/// Is the named lock held by some process right now? Does not take it.
pub fn lock_held(name: &str) -> bool {
    platform::lock_held(name)
}

/// Next step while taking the engine lock.
#[derive(Debug, PartialEq, Eq)]
pub enum LockNext {
    Proceed,
    Wait,
    StopOthersAndRetry,
    GiveUp,
}

/// Decide the next step: wait up to `LOCK_WAIT` for the holder, then stop
/// the other instances once more and retry once, then give up.
pub fn lock_next(acquired: bool, waited: Duration, stopped_again: bool) -> LockNext {
    if acquired {
        LockNext::Proceed
    } else if stopped_again {
        LockNext::GiveUp
    } else if waited < LOCK_WAIT {
        LockNext::Wait
    } else {
        LockNext::StopOthersAndRetry
    }
}

/// Held for the life of the engine; released on drop or process exit.
pub struct EngineLock(#[allow(dead_code)] Option<platform::Lock>);

/// Take the engine lock (call right after `stop_others`). `Err` means another
/// Page2Cam holds it even after stopping the others again: this one must
/// exit. If the lock itself can't be created, run without it.
pub fn take_engine_lock(me: u32, force: Option<u32>, keep: &[u32]) -> anyhow::Result<EngineLock> {
    let begun = Instant::now();
    let mut stopped_again = false;
    let mut said_waiting = false;
    loop {
        let got = match platform::try_lock(ENGINE_LOCK) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("cannot create the engine lock, running without it: {e:#}");
                return Ok(EngineLock(None));
            }
        };
        match lock_next(got.is_some(), begun.elapsed(), stopped_again) {
            LockNext::Proceed => return Ok(EngineLock(got)),
            LockNext::Wait => {
                if !said_waiting {
                    tracing::info!("another Page2Cam holds the engine lock; waiting up to 3 s");
                    said_waiting = true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            LockNext::StopOthersAndRetry => {
                tracing::warn!(
                    "engine lock still held after 3 s; stopping other Page2Cam processes again"
                );
                stop_others(me, force, keep);
                stopped_again = true;
            }
            LockNext::GiveUp => anyhow::bail!("another Page2Cam is starting"),
        }
    }
}

fn is_page2cam(name: &str) -> bool {
    if cfg!(windows) {
        name.eq_ignore_ascii_case("page2cam.exe")
    } else {
        name == "page2cam"
    }
}

/// The parent chain of `pid` (excluding `pid`), at most `MAX_CHAIN` long.
fn ancestors(procs: &[Proc], pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut cur = pid;
    for _ in 0..MAX_CHAIN {
        let Some(p) = procs.iter().find(|p| p.pid == cur) else {
            break;
        };
        if p.parent == 0 || p.parent == cur || p.parent == pid || out.contains(&p.parent) {
            break;
        }
        out.push(p.parent);
        cur = p.parent;
    }
    out
}

/// Pids of every Page2Cam process to stop: all named page2cam except `me`,
/// descendants of `me` (our CEF helpers) and ancestors of `me`, except `force`
/// (the old instance of a restart, which is our parent but must go if it hung).
pub fn to_stop(procs: &[Proc], me: u32, force: Option<u32>) -> Vec<u32> {
    to_stop_keeping(procs, me, force, &[])
}

/// [`to_stop`], also sparing the pids in `keep` (engine mode: the tray that
/// started us, even if the parent chain doesn't lead to it).
pub fn to_stop_keeping(procs: &[Proc], me: u32, force: Option<u32>, keep: &[u32]) -> Vec<u32> {
    let keep_up: Vec<u32> = ancestors(procs, me)
        .into_iter()
        .filter(|a| Some(*a) != force)
        .chain(keep.iter().copied())
        .collect();
    let my_start = procs.iter().find(|p| p.pid == me).and_then(|p| p.started);
    let racing = |p: &Proc| match (p.started, my_start) {
        (Some(s), Some(m)) => s >= m && s - m <= RACE_WINDOW,
        _ => false,
    };
    procs
        .iter()
        .filter(|p| is_page2cam(&p.name) && !p.mcp_bridge)
        .filter(|p| p.pid != me && !keep_up.contains(&p.pid))
        .filter(|p| !ancestors(procs, p.pid).contains(&me))
        .filter(|p| Some(p.pid) == force || !racing(p))
        .map(|p| p.pid)
        .collect()
}

/// The tray's leftover rule: every Page2Cam process except the tray (`me`)
/// and its ancestors. Unlike [`to_stop`], its own descendants (an engine it
/// gave up on, and that engine's helpers) are stopped too, and there is no
/// race window (the tray holds its lock; a racing launch backs off itself).
pub fn to_stop_for_tray(procs: &[Proc], me: u32) -> Vec<u32> {
    let up = ancestors(procs, me);
    procs
        .iter()
        .filter(|p| is_page2cam(&p.name) && !p.mcp_bridge)
        .filter(|p| p.pid != me && !up.contains(&p.pid))
        .map(|p| p.pid)
        .collect()
}

/// Every running process (pid, parent, name; path and start time for
/// page2cam processes).
pub fn list_processes() -> Vec<Proc> {
    platform::list()
}

/// Stop all other Page2Cam processes except those in `keep`; returns the
/// pids that were targeted.
pub fn stop_others(me: u32, force: Option<u32>, keep: &[u32]) -> Vec<u32> {
    let procs = platform::list();
    let pids = to_stop_keeping(&procs, me, force, keep);
    stop_pids(&procs, pids)
}

/// The tray's leftover stop ([`to_stop_for_tray`]); returns the pids targeted.
pub fn stop_others_for_tray(me: u32) -> Vec<u32> {
    let procs = platform::list();
    let pids = to_stop_for_tray(&procs, me);
    stop_pids(&procs, pids)
}

fn stop_pids(procs: &[Proc], pids: Vec<u32>) -> Vec<u32> {
    if pids.is_empty() {
        return pids;
    }
    let path_of = |pid: u32| {
        procs
            .iter()
            .find(|p| p.pid == pid)
            .and_then(|p| p.path.clone())
    };
    for &pid in &pids {
        tracing::warn!(pid, path = ?path_of(pid), "stopping a leftover Page2Cam process");
    }
    platform::stop(&pids, WAIT_EXIT);
    let after = platform::list();
    for &pid in &pids {
        if after.iter().any(|p| p.pid == pid && is_page2cam(&p.name)) {
            tracing::warn!(pid, path = ?path_of(pid), "leftover Page2Cam process is still running");
        } else {
            tracing::info!(pid, "stopped the leftover Page2Cam process");
        }
    }
    pids
}

#[cfg(windows)]
mod platform {
    use super::Proc;
    use std::path::PathBuf;
    use std::time::Duration;
    use windows::core::{HSTRING, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, FILETIME, HANDLE,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        CreateMutexW, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
        WaitForSingleObject, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    /// FILETIME units (100 ns).
    pub const TICKS_PER_SEC: u64 = 10_000_000;

    /// Process creation time as FILETIME ticks.
    fn creation_time(h: HANDLE) -> Option<u64> {
        let (mut c, mut e, mut k, mut u) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).ok()? };
        Some(((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64)
    }

    /// The engine lock: a named mutex that exists while some process holds
    /// a handle to it (handles are not inherited and close on exit).
    pub struct Lock(HANDLE);

    // SAFETY: a mutex handle is usable from any thread; we only close it.
    unsafe impl Send for Lock {}

    impl Drop for Lock {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// `Ok(None)` when another process (or handle) already holds `name`.
    pub fn try_lock(name: &str) -> anyhow::Result<Option<Lock>> {
        let full = HSTRING::from(format!("Local\\{name}"));
        unsafe {
            let h = CreateMutexW(None, false, &full)?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(h);
                return Ok(None);
            }
            Ok(Some(Lock(h)))
        }
    }

    /// Does a mutex `Local\<name>` exist (someone holds a handle to it)?
    pub fn lock_held(name: &str) -> bool {
        use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
        let full = HSTRING::from(format!("Local\\{name}"));
        match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, &full) } {
            Ok(h) => {
                unsafe {
                    let _ = CloseHandle(h);
                }
                true
            }
            Err(_) => false,
        }
    }

    fn image_path(h: HANDLE) -> Option<PathBuf> {
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
                .ok()?;
        }
        Some(PathBuf::from(String::from_utf16_lossy(
            &buf[..len as usize],
        )))
    }

    pub fn list() -> Vec<Proc> {
        let mut out = Vec::new();
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                tracing::warn!("could not list processes");
                return out;
            };
            let mut e = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut ok = Process32FirstW(snap, &mut e).is_ok();
            while ok {
                let n = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(0);
                let name = String::from_utf16_lossy(&e.szExeFile[..n]);
                let mut path = None;
                let mut started = None;
                if name.eq_ignore_ascii_case("page2cam.exe") {
                    if let Ok(h) =
                        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, e.th32ProcessID)
                    {
                        path = image_path(h);
                        started = creation_time(h);
                        let _ = CloseHandle(h);
                    }
                }
                // A bridge holds `Local\Page2Cam.Mcp.<pid>` for its life.
                let mcp_bridge = name.eq_ignore_ascii_case("page2cam.exe")
                    && lock_held(&super::mcp_lock_name(e.th32ProcessID));
                out.push(Proc {
                    pid: e.th32ProcessID,
                    parent: e.th32ParentProcessID,
                    name,
                    path,
                    started,
                    mcp_bridge,
                });
                ok = Process32NextW(snap, &mut e).is_ok();
            }
            let _ = CloseHandle(snap);
        }
        out
    }

    pub fn stop(pids: &[u32], wait: Duration) {
        for &pid in pids {
            unsafe {
                let access =
                    PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
                match OpenProcess(access, false, pid) {
                    Ok(h) => {
                        if let Err(e) = TerminateProcess(h, 1) {
                            tracing::warn!(pid, error = %e, "could not stop the process");
                        } else {
                            WaitForSingleObject(h, wait.as_millis() as u32);
                        }
                        let _ = CloseHandle(h);
                    }
                    Err(e) => {
                        tracing::warn!(pid, error = %e, "could not open the process to stop it")
                    }
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Proc;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// Clock ticks since boot (`/proc/<pid>/stat` starttime); USER_HZ is 100
    /// on every mainstream Linux build.
    pub const TICKS_PER_SEC: u64 = 100;

    /// The engine lock: a flock'd file (released when the process exits; the
    /// descriptor is close-on-exec, so CEF helpers don't keep it).
    pub struct Lock(#[allow(dead_code)] std::fs::File);

    fn lock_dir() -> Option<PathBuf> {
        if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(d));
        }
        let d = directories::ProjectDirs::from("", "", "page2cam")?
            .data_dir()
            .to_path_buf();
        std::fs::create_dir_all(&d).ok()?;
        Some(d)
    }

    /// `Ok(None)` when another process (or descriptor) already holds `name`.
    pub fn try_lock(name: &str) -> anyhow::Result<Option<Lock>> {
        let dir = lock_dir().ok_or_else(|| anyhow::anyhow!("no runtime or data directory"))?;
        let path = dir.join(format!("{name}.lock"));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| anyhow::anyhow!("opening {}: {e}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Lock(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(anyhow::anyhow!("locking {}: {e}", path.display()))
            }
        }
    }

    pub fn lock_held(name: &str) -> bool {
        matches!(try_lock(name), Ok(None))
    }

    pub fn list() -> Vec<Proc> {
        let mut out = Vec::new();
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return out;
        };
        for entry in dir.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            // "pid (comm) S ppid ..." where comm may contain spaces/parens.
            let (Some(l), Some(r)) = (stat.find('('), stat.rfind(')')) else {
                continue;
            };
            let name = stat[l + 1..r].to_string();
            // Fields after "(comm)": state ppid ... starttime is field 22 (index 19).
            let fields: Vec<&str> = stat[r + 1..].split_whitespace().collect();
            let parent = fields.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let started = fields.get(19).and_then(|s| s.parse().ok());
            let path: Option<PathBuf> = std::fs::read_link(entry.path().join("exe")).ok();
            // The command line, NUL-separated.
            let mcp_bridge = name == "page2cam"
                && std::fs::read(entry.path().join("cmdline")).is_ok_and(|c| {
                    let args: Vec<&[u8]> =
                        c.split(|b| *b == 0).filter(|a| !a.is_empty()).collect();
                    let args: Vec<String> = args
                        .iter()
                        .map(|a| String::from_utf8_lossy(a).into_owned())
                        .collect();
                    super::is_mcp_args(&args)
                });
            out.push(Proc {
                pid,
                parent,
                name,
                path,
                started,
                mcp_bridge,
            });
        }
        out
    }

    fn alive(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }

    fn signal(sig: &str, pids: &[u32]) {
        for pid in pids {
            let _ = std::process::Command::new("kill")
                .arg(format!("-{sig}"))
                .arg(pid.to_string())
                .status();
        }
    }

    pub fn stop(pids: &[u32], wait: Duration) {
        signal("TERM", pids);
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline && pids.iter().any(|p| alive(*p)) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let left: Vec<u32> = pids.iter().copied().filter(|p| alive(*p)).collect();
        signal("KILL", &left);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe() -> &'static str {
        if cfg!(windows) {
            "page2cam.exe"
        } else {
            "page2cam"
        }
    }

    fn p(pid: u32, parent: u32, name: &str) -> Proc {
        Proc {
            pid,
            parent,
            name: name.to_string(),
            path: None,
            started: None,
            mcp_bridge: false,
        }
    }

    fn bridge(mut proc: Proc) -> Proc {
        proc.mcp_bridge = true;
        proc
    }

    #[test]
    fn bridges_are_never_stopped() {
        // An agent's `page2cam --mcp` (pid 30) next to an old engine (pid 10):
        // starting an engine (20) or the tray's sweep (5) stops 10 only.
        let procs = [
            p(5, 1, exe()),
            p(10, 1, exe()),
            p(20, 5, exe()),
            bridge(p(30, 77, exe())),
        ];
        assert_eq!(to_stop(&procs, 20, None), vec![10]);
        assert_eq!(to_stop_keeping(&procs, 20, None, &[5]), vec![10]);
        assert_eq!(to_stop_for_tray(&procs, 5), vec![10, 20]);
    }

    #[test]
    fn mcp_args_are_exactly_dash_dash_mcp() {
        assert!(is_mcp_args(&["page2cam.exe", "--mcp"]));
        assert!(!is_mcp_args(&["page2cam.exe"]));
        assert!(!is_mcp_args(&["page2cam.exe", "--engine", "--mcp"]));
        assert!(!is_mcp_args(&["page2cam.exe", "--mcpx"]));
        assert_eq!(mcp_lock_name(42), "Page2Cam.Mcp.42");
    }

    fn started(mut proc: Proc, at: u64) -> Proc {
        proc.started = Some(at);
        proc
    }

    #[test]
    fn spares_instance_started_just_after_us() {
        // Two launches milliseconds apart: only the newer stops the older.
        let base = 1_000_000;
        let older = started(p(10, 1, exe()), base);
        let newer = started(p(20, 1, exe()), base + 1);
        let procs = [older, newer];
        assert!(to_stop(&procs, 10, None).is_empty());
        assert_eq!(to_stop(&procs, 20, None), vec![10]);
    }

    #[test]
    fn spares_instance_started_in_the_same_tick() {
        let procs = [started(p(10, 1, exe()), 500), started(p(20, 1, exe()), 500)];
        // Neither stops the other; the engine lock decides who runs.
        assert_eq!(to_stop(&procs, 20, None), vec![] as Vec<u32>);
        assert_eq!(to_stop(&procs, 10, None), vec![] as Vec<u32>);
    }

    #[test]
    fn stops_newer_looking_instance_beyond_race_window() {
        // A start time far "after" ours is a clock jump, not a racing launch.
        let procs = [
            started(p(10, 1, exe()), 100 + RACE_WINDOW + 1),
            started(p(20, 1, exe()), 100),
        ];
        assert_eq!(to_stop(&procs, 20, None), vec![10]);
    }

    #[test]
    fn force_pid_is_stopped_even_if_newer() {
        let procs = [
            started(p(10, 1, exe()), 101),
            started(p(20, 10, exe()), 100),
        ];
        assert_eq!(to_stop(&procs, 20, Some(10)), vec![10]);
    }

    #[test]
    fn engine_lock_steps() {
        let s = Duration::from_secs;
        assert_eq!(lock_next(true, s(0), false), LockNext::Proceed);
        assert_eq!(lock_next(true, s(9), true), LockNext::Proceed);
        assert_eq!(lock_next(false, s(0), false), LockNext::Wait);
        assert_eq!(
            lock_next(false, Duration::from_millis(2_900), false),
            LockNext::Wait
        );
        assert_eq!(lock_next(false, s(3), false), LockNext::StopOthersAndRetry);
        assert_eq!(lock_next(false, s(3), true), LockNext::GiveUp);
        assert_eq!(lock_next(false, s(0), true), LockNext::GiveUp);
    }

    #[test]
    fn engine_lock_is_exclusive_within_the_machine() {
        // Own name, so a running Page2Cam doesn't interfere.
        let name = format!("Page2Cam.Test.{}", std::process::id());
        let a = platform::try_lock(&name).unwrap();
        assert!(a.is_some(), "first lock must succeed");
        assert!(lock_held(&name), "seen as held");
        assert!(
            platform::try_lock(&name).unwrap().is_none(),
            "second must see it held"
        );
        drop(a);
        assert!(!lock_held(&name), "not held after drop");
        assert!(
            platform::try_lock(&name).unwrap().is_some(),
            "released on drop"
        );
    }

    #[test]
    fn engine_keeps_its_tray_even_when_the_chain_is_broken() {
        // Engine 20 started by tray 10; its recorded parent is a broker (5),
        // so the tray is not an ancestor. `keep` still protects it, and
        // only it: another leftover (30) is stopped.
        let procs = [
            p(10, 1, exe()),
            p(5, 1, "broker.exe"),
            p(20, 5, exe()),
            p(30, 1, exe()),
        ];
        assert_eq!(to_stop_keeping(&procs, 20, None, &[10]), vec![30]);
        assert_eq!(to_stop(&procs, 20, None), vec![10, 30]);
        // An ancestor tray is kept as before.
        let procs = [p(10, 1, exe()), p(20, 10, exe())];
        assert!(to_stop_keeping(&procs, 20, None, &[10]).is_empty());
    }

    #[test]
    fn tray_stops_its_own_engine_tree_but_not_its_ancestors() {
        // explorer(1) -> launcher page2cam(5) -> tray(10) -> engine(20) ->
        // helper(21); plus an unrelated leftover (30) started right after
        // the tray (no race window for the tray: it holds its lock).
        let procs = [
            p(1, 0, "explorer.exe"),
            started(p(5, 1, exe()), 90),
            started(p(10, 5, exe()), 100),
            started(p(20, 10, exe()), 101),
            started(p(21, 20, exe()), 102),
            started(p(30, 1, exe()), 103),
        ];
        assert_eq!(to_stop_for_tray(&procs, 10), vec![20, 21, 30]);
        // The engine-side rule spares descendants; the tray's does not.
        assert_eq!(to_stop(&procs, 10, None), vec![] as Vec<u32>);
    }

    #[test]
    fn stops_other_main_instance() {
        let procs = [p(1, 0, "explorer"), p(10, 1, exe()), p(20, 1, exe())];
        assert_eq!(to_stop(&procs, 20, None), vec![10]);
    }

    #[test]
    fn keeps_own_helpers() {
        let procs = [p(20, 1, exe()), p(21, 20, exe()), p(22, 21, exe())];
        assert!(to_stop(&procs, 20, None).is_empty());
    }

    #[test]
    fn force_pid_is_stopped_even_as_parent() {
        let procs = [p(10, 1, exe()), p(20, 10, exe())];
        assert_eq!(to_stop(&procs, 20, Some(10)), vec![10]);
        assert!(to_stop(&procs, 20, None).is_empty());
    }

    #[test]
    fn keeps_own_ancestors() {
        let procs = [p(5, 1, exe()), p(6, 5, exe()), p(20, 6, exe())];
        assert!(to_stop(&procs, 20, None).is_empty());
    }

    #[test]
    fn survives_parent_cycles() {
        let procs = [p(5, 6, exe()), p(6, 5, exe()), p(20, 5, exe())];
        // Must terminate; the cycle members are our ancestors, so kept.
        assert!(to_stop(&procs, 20, None).is_empty());
    }

    #[test]
    fn stops_orphaned_helpers_of_dead_instance() {
        let procs = [p(20, 1, exe()), p(30, 999, exe())];
        assert_eq!(to_stop(&procs, 20, None), vec![30]);
    }

    #[test]
    fn ignores_other_programs() {
        let procs = [
            p(20, 1, exe()),
            p(30, 1, "chrome.exe"),
            p(31, 1, "page2cam-tool"),
        ];
        assert!(to_stop(&procs, 20, None).is_empty());
    }

    #[test]
    #[cfg(windows)]
    fn name_match_is_case_insensitive_on_windows() {
        let procs = [p(20, 1, "page2cam.exe"), p(30, 1, "Page2Cam.EXE")];
        assert_eq!(to_stop(&procs, 20, None), vec![30]);
    }
}
