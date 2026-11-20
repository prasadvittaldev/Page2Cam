//! Keep Page2Cam out of Windows power throttling (EcoQoS).
//!
//! Windows treats a process without a foreground window as a background app
//! and may run it on efficiency cores at a low clock ("EcoQoS"). The engine
//! renders offscreen, so it and its Chromium helpers (GPU process, renderers,
//! utility) look like background apps; a live monitor saw the CPU clock fall
//! to 25–60 % and one renderer at Idle priority with EcoQoS. We opt the
//! engine out at start ([`opt_out_self`]) and its helpers as they appear
//! ([`ChildOptOut::sweep`], every 30 s). Process priority classes are left
//! alone. Linux: no-ops.

// Linux: the bookkeeping is only exercised by the tests.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::path::Path;

use p2c_proc::Proc;

/// Maximum parent-chain steps followed (guards against pid-reuse cycles).
const MAX_CHAIN: usize = 64;

/// Which descendants of `root` get the opt-out, remembering who already had
/// it. Pure bookkeeping; the platform calls are in [`ChildOptOut`].
pub struct ChildTracker {
    root: u32,
    done: HashSet<u32>,
}

impl ChildTracker {
    pub fn new(root: u32) -> Self {
        Self {
            root,
            done: HashSet::new(),
        }
    }

    /// Pids to opt out now, from a process list: descendants of the root
    /// (children, and their children) that run our exe (`our_exe`; by file
    /// name when a path is unknown) or ffmpeg (the stream encoder), and were
    /// not handled before. They are marked handled. Pids that are gone are
    /// forgotten, so a reused pid is handled again.
    pub fn pending(&mut self, procs: &[Proc], our_exe: &Path) -> Vec<u32> {
        let parent: HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.parent)).collect();
        self.done.retain(|pid| parent.contains_key(pid));
        let our_name = our_exe.file_name().map(|n| n.to_string_lossy());
        let ours = |p: &Proc| match &p.path {
            Some(path) => same_path(path, our_exe),
            None => our_name
                .as_deref()
                .is_some_and(|n| p.name.eq_ignore_ascii_case(n)),
        };
        let mut out: Vec<u32> = procs
            .iter()
            .filter(|p| p.pid != self.root && !self.done.contains(&p.pid))
            .filter(|p| is_ffmpeg(&p.name) || ours(p))
            .filter(|p| descends_from(&parent, p.pid, self.root))
            .map(|p| p.pid)
            .collect();
        out.sort_unstable();
        out.dedup();
        self.done.extend(&out);
        out
    }
}

fn is_ffmpeg(name: &str) -> bool {
    name.eq_ignore_ascii_case("ffmpeg.exe") || name == "ffmpeg"
}

/// Windows paths: case-insensitive.
fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

/// Is `root` on the parent chain of `pid` (at most `MAX_CHAIN` steps)?
fn descends_from(parent: &HashMap<u32, u32>, pid: u32, root: u32) -> bool {
    let mut cur = pid;
    for _ in 0..MAX_CHAIN {
        match parent.get(&cur) {
            Some(&up) if up == root => return true,
            Some(&up) if up != 0 && up != cur => cur = up,
            _ => return false,
        }
    }
    false
}

/// Opt this process out of power throttling and log the outcome once.
pub fn opt_out_self() {
    #[cfg(windows)]
    match platform::opt_out_current() {
        Ok(()) => tracing::info!("power throttling (EcoQoS) off for this process"),
        Err(e) => tracing::warn!("could not turn power throttling off for this process: {e}"),
    }
}

/// Opts the engine's helper processes out of power throttling as they
/// appear. Call [`ChildOptOut::sweep`] periodically (every 30 s).
pub struct ChildOptOut {
    tracker: ChildTracker,
    exe: Option<std::path::PathBuf>,
}

impl Default for ChildOptOut {
    /// For this process's helpers.
    fn default() -> Self {
        Self {
            tracker: ChildTracker::new(std::process::id()),
            exe: std::env::current_exe().ok(),
        }
    }
}

impl ChildOptOut {
    /// One Toolhelp snapshot; the opt-out for every new helper. Logs which
    /// ones were done (only when there are new ones).
    pub fn sweep(&mut self) {
        #[cfg(windows)]
        {
            let Some(exe) = &self.exe else { return };
            let procs = p2c_proc::list_processes();
            let pids = self.tracker.pending(&procs, exe);
            if pids.is_empty() {
                return;
            }
            let mut done = Vec::new();
            for pid in pids {
                let name = procs
                    .iter()
                    .find(|p| p.pid == pid)
                    .map_or("", |p| p.name.as_str());
                match platform::opt_out_pid(pid) {
                    Ok(()) => done.push(pid),
                    // Once per pid (it is marked handled either way).
                    Err(e) => tracing::warn!(
                        pid,
                        name,
                        "could not turn power throttling off for a helper process: {e}"
                    ),
                }
            }
            if !done.is_empty() {
                tracing::info!(pids = ?done, "power throttling (EcoQoS) off for helper processes");
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, ProcessPowerThrottling, SetProcessInformation,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE, PROCESS_SET_INFORMATION,
    };

    /// ControlMask = EXECUTION_SPEED, StateMask = 0: "never throttle my
    /// execution speed" (opted out of EcoQoS).
    fn opt_out(h: HANDLE) -> windows::core::Result<()> {
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: 0,
        };
        // SAFETY: `state` is a valid PROCESS_POWER_THROTTLING_STATE that
        // outlives the call, and the size passed is its size.
        unsafe {
            SetProcessInformation(
                h,
                ProcessPowerThrottling,
                std::ptr::from_ref(&state).cast(),
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        }
    }

    pub fn opt_out_current() -> windows::core::Result<()> {
        // SAFETY: the pseudo handle needs no closing.
        opt_out(unsafe { GetCurrentProcess() })
    }

    pub fn opt_out_pid(pid: u32) -> windows::core::Result<()> {
        // SAFETY: a handle we own, closed below.
        let h = unsafe { OpenProcess(PROCESS_SET_INFORMATION, false, pid)? };
        let r = opt_out(h);
        unsafe {
            let _ = CloseHandle(h);
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const ENGINE: u32 = 100;

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Program Files\Page2Cam\page2cam.exe")
    }

    fn p(pid: u32, parent: u32, name: &str, path: Option<&str>) -> Proc {
        Proc {
            pid,
            parent,
            name: name.into(),
            path: path.map(PathBuf::from),
            started: None,
            mcp_bridge: false,
        }
    }

    fn ours(pid: u32, parent: u32) -> Proc {
        p(
            pid,
            parent,
            "page2cam.exe",
            Some(r"C:\Program Files\Page2Cam\page2cam.exe"),
        )
    }

    #[test]
    fn picks_our_helpers_once() {
        let procs = [
            p(4, 0, "System", None),
            p(10, 4, "explorer.exe", None),
            ours(50, 10),      // the tray
            ours(ENGINE, 50),  // the engine itself: done at start, not here
            ours(101, ENGINE), // gpu-process
            ours(102, ENGINE), // renderer
        ];
        let mut t = ChildTracker::new(ENGINE);
        assert_eq!(t.pending(&procs, &exe()), vec![101, 102]);
        // Already handled: nothing on the next sweep.
        assert!(t.pending(&procs, &exe()).is_empty());
    }

    #[test]
    fn helpers_that_appear_later_are_picked_up() {
        let mut t = ChildTracker::new(ENGINE);
        let first = [ours(ENGINE, 1), ours(101, ENGINE)];
        assert_eq!(t.pending(&first, &exe()), vec![101]);
        // A new renderer (navigation to another site) and a grandchild.
        let later = [
            ours(ENGINE, 1),
            ours(101, ENGINE),
            ours(103, ENGINE),
            ours(104, 103),
        ];
        assert_eq!(t.pending(&later, &exe()), vec![103, 104]);
    }

    #[test]
    fn leaves_unrelated_processes_alone() {
        let procs = [
            ours(ENGINE, 1),
            // Our children running other programs.
            p(
                110,
                ENGINE,
                "conhost.exe",
                Some(r"C:\Windows\System32\conhost.exe"),
            ),
            p(
                111,
                ENGINE,
                "curl.exe",
                Some(r"C:\Windows\System32\curl.exe"),
            ),
            // Another copy of page2cam elsewhere, not our descendant.
            ours(200, 1),
            p(
                201,
                200,
                "page2cam.exe",
                Some(r"C:\Program Files\Page2Cam\page2cam.exe"),
            ),
            // Same file name as ours, but a different exe.
            p(112, ENGINE, "page2cam.exe", Some(r"D:\other\page2cam.exe")),
        ];
        let mut t = ChildTracker::new(ENGINE);
        assert!(t.pending(&procs, &exe()).is_empty());
    }

    #[test]
    fn ffmpeg_child_gets_the_opt_out() {
        let procs = [
            ours(ENGINE, 1),
            p(
                120,
                ENGINE,
                "ffmpeg.exe",
                Some(r"C:\Program Files\Page2Cam\ffmpeg.exe"),
            ),
            p(121, ENGINE, "FFMPEG.EXE", None),
        ];
        let mut t = ChildTracker::new(ENGINE);
        assert_eq!(t.pending(&procs, &exe()), vec![120, 121]);
    }

    #[test]
    fn path_match_ignores_case_and_unknown_path_falls_back_to_the_name() {
        let procs = [
            ours(ENGINE, 1),
            p(
                130,
                ENGINE,
                "PAGE2CAM.EXE",
                Some(r"c:\program files\page2cam\PAGE2CAM.exe"),
            ),
            // Path unknown (could not be queried): our file name is enough
            // for a descendant.
            p(131, ENGINE, "page2cam.exe", None),
        ];
        let mut t = ChildTracker::new(ENGINE);
        assert_eq!(t.pending(&procs, &exe()), vec![130, 131]);
    }

    #[test]
    fn a_reused_pid_is_handled_again() {
        let mut t = ChildTracker::new(ENGINE);
        assert_eq!(
            t.pending(&[ours(ENGINE, 1), ours(101, ENGINE)], &exe()),
            vec![101]
        );
        // 101 exits...
        assert!(t.pending(&[ours(ENGINE, 1)], &exe()).is_empty());
        // ...and its pid comes back as a new renderer.
        assert_eq!(
            t.pending(&[ours(ENGINE, 1), ours(101, ENGINE)], &exe()),
            vec![101]
        );
    }

    #[test]
    fn survives_parent_cycles() {
        // Pid reuse can make a parent chain loop; it must end, and the loop
        // is not our descendant.
        let procs = [
            ours(ENGINE, 1),
            ours(300, 301),
            ours(301, 300),
            ours(101, ENGINE),
        ];
        let mut t = ChildTracker::new(ENGINE);
        assert_eq!(t.pending(&procs, &exe()), vec![101]);
    }
}
