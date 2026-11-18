//! Restarting Page2Cam: a plain self-relaunch (same exe, same command line)
//! for "Restart now" in the admin page, and after Chromium dies unexpectedly
//! (e.g. a driver update changed the adapter LUID it was started with).
//!
//! The new process gets `--wait-for-pid <old pid>` and waits for the old one
//! to exit before it opens the camera and the admin port.

use std::time::Duration;

/// Hidden flag: wait for this process to exit first.
pub const WAIT_FOR_PID: &str = "--wait-for-pid";
/// Hidden flag: unexpected browser exits so far (R28).
pub const UNEXPECTED_EXIT_COUNT: &str = "--unexpected-exit-count";
/// Hidden flag: `gpu` setting for this run only (not saved).
pub const GPU_OVERRIDE: &str = "--gpu-override";

/// How long a relaunched process waits for the old one.
pub const WAIT_FOR_OLD: Duration = Duration::from_secs(10);
/// A second unexpected exit this soon after start means the GPU path is
/// broken: the next run uses the CPU.
pub const QUICK_EXIT: Duration = Duration::from_secs(60);

/// Why the browser loop is ending, set before asking it to shut down.
#[derive(Default)]
pub struct ExitFlags {
    /// Ctrl-C, console window closed, or closing for an update.
    pub user_shutdown: std::sync::atomic::AtomicBool,
    /// "Restart now".
    pub restart: std::sync::atomic::AtomicBool,
}

/// What to do once the browser loop has returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitAction {
    /// Stay closed.
    Exit,
    /// Start again. `unexpected_count` goes to the new process (0: none);
    /// `gpu_cpu` adds the one-run `--gpu-override cpu`.
    Relaunch {
        unexpected_count: u32,
        gpu_cpu: bool,
    },
}

/// R28: decide after the browser loop returned. `user_shutdown`: Ctrl-C,
/// window close or an update; `restart_requested`: "Restart now";
/// `unexpected_count`: this run's `--unexpected-exit-count`; `uptime`: since
/// this process started.
pub fn after_browser_exit(
    user_shutdown: bool,
    restart_requested: bool,
    unexpected_count: u32,
    uptime: Duration,
) -> ExitAction {
    let relaunch = |unexpected_count, gpu_cpu| ExitAction::Relaunch {
        unexpected_count,
        gpu_cpu,
    };
    if restart_requested {
        return relaunch(0, false);
    }
    if user_shutdown {
        return ExitAction::Exit;
    }
    match unexpected_count {
        // Ran a while: a fresh start with a fresh adapter list.
        _ if uptime >= QUICK_EXIT => relaunch(1, false),
        0 => relaunch(1, false),
        // Again, right after the relaunch: once more, without the GPU.
        1 => relaunch(2, true),
        // Even the CPU run died at once: something else is wrong.
        _ => ExitAction::Exit,
    }
}

/// `args` without `flag <value>` / `flag=<value>`.
fn without_flag(args: &[String], flag: &str) -> Vec<String> {
    let with_eq = format!("{flag}=");
    let mut out = Vec::with_capacity(args.len());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            it.next(); // its value
        } else if !a.starts_with(&with_eq) {
            out.push(a.clone());
        }
    }
    out
}

/// `current` (without the program name) for the new process: any old
/// `--wait-for-pid` dropped, `--wait-for-pid <pid>` appended.
pub fn relaunch_args(current: &[String], pid: u32) -> Vec<String> {
    let mut v = without_flag(current, WAIT_FOR_PID);
    v.extend([WAIT_FOR_PID.to_string(), pid.to_string()]);
    v
}

/// The new process's arguments for `action`: the old hidden R28 flags are
/// replaced by the ones `action` asks for (none for "Restart now", so the
/// saved GPU setting applies again).
pub fn args_for(action: ExitAction, current: &[String], pid: u32) -> Vec<String> {
    let mut v = without_flag(&without_flag(current, UNEXPECTED_EXIT_COUNT), GPU_OVERRIDE);
    if let ExitAction::Relaunch {
        unexpected_count,
        gpu_cpu,
    } = action
    {
        if unexpected_count > 0 {
            v.extend([
                UNEXPECTED_EXIT_COUNT.to_string(),
                unexpected_count.to_string(),
            ]);
        }
        if gpu_cpu {
            v.extend([GPU_OVERRIDE.to_string(), "cpu".to_string()]);
        }
    }
    relaunch_args(&v, pid)
}

/// Wait until process `pid` has exited, at most `timeout`. True if it has
/// (or can't be found).
#[cfg(windows)]
pub fn wait_for_pid(pid: u32, timeout: Duration) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    // SAFETY: plain Win32 calls; the handle is closed before returning.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return true; // gone already
        };
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        let r = WaitForSingleObject(h, ms);
        let _ = CloseHandle(h);
        r != WAIT_TIMEOUT
    }
}

#[cfg(not(windows))]
pub fn wait_for_pid(pid: u32, timeout: Duration) -> bool {
    // Running unless /proc has no entry or it is a zombie (exited, unreaped).
    let alive = || {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
            s.rsplit_once(')')
                .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z'))
        })
    };
    let start = std::time::Instant::now();
    while alive() {
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// Start `exe args` and don't wait for it.
///
/// Windows: with a console (the usual case: page2cam is a console app), the
/// new process inherits it, so it keeps the window and Ctrl-C after the old
/// process exits. Without one (started detached), it is started detached
/// too, so no console window pops up.
pub fn spawn(exe: &std::path::Path, args: &[String]) -> anyhow::Result<()> {
    use anyhow::Context;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        // SAFETY: no arguments; returns a window handle or null.
        let has_console = !unsafe { windows::Win32::System::Console::GetConsoleWindow() }
            .0
            .is_null();
        if !has_console {
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }
    }
    cmd.spawn()
        .with_context(|| format!("starting {}", exe.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn relaunch_args_append_wait_pid() {
        assert_eq!(
            relaunch_args(&s(&["--url", "https://a.test", "--no-open"]), 4242),
            s(&[
                "--url",
                "https://a.test",
                "--no-open",
                "--wait-for-pid",
                "4242"
            ])
        );
        assert_eq!(relaunch_args(&[], 7), s(&["--wait-for-pid", "7"]));
    }

    #[test]
    fn relaunch_args_replace_old_wait_pid() {
        assert_eq!(
            relaunch_args(&s(&["--wait-for-pid", "11", "--fps", "25"]), 12),
            s(&["--fps", "25", "--wait-for-pid", "12"])
        );
        assert_eq!(
            relaunch_args(&s(&["--fps", "25", "--wait-for-pid=11"]), 12),
            s(&["--fps", "25", "--wait-for-pid", "12"])
        );
    }

    const SOON: Duration = Duration::from_secs(5);
    const LATER: Duration = Duration::from_secs(3600);

    #[test]
    fn user_shutdown_stays_closed() {
        assert_eq!(after_browser_exit(true, false, 0, SOON), ExitAction::Exit);
        assert_eq!(after_browser_exit(true, false, 1, SOON), ExitAction::Exit);
    }

    #[test]
    fn restart_now_relaunches_plainly() {
        // "Restart now" also shuts the browser down; it wins.
        let plain = ExitAction::Relaunch {
            unexpected_count: 0,
            gpu_cpu: false,
        };
        assert_eq!(after_browser_exit(true, true, 0, SOON), plain);
        assert_eq!(after_browser_exit(false, true, 1, SOON), plain);
    }

    #[test]
    fn first_unexpected_exit_relaunches() {
        assert_eq!(
            after_browser_exit(false, false, 0, SOON),
            ExitAction::Relaunch {
                unexpected_count: 1,
                gpu_cpu: false
            }
        );
    }

    #[test]
    fn second_quick_unexpected_exit_relaunches_on_cpu() {
        assert_eq!(
            after_browser_exit(false, false, 1, Duration::from_secs(59)),
            ExitAction::Relaunch {
                unexpected_count: 2,
                gpu_cpu: true
            }
        );
    }

    #[test]
    fn slow_second_exit_counts_as_a_first() {
        assert_eq!(
            after_browser_exit(false, false, 1, LATER),
            ExitAction::Relaunch {
                unexpected_count: 1,
                gpu_cpu: false
            }
        );
        assert_eq!(
            after_browser_exit(false, false, 2, LATER),
            ExitAction::Relaunch {
                unexpected_count: 1,
                gpu_cpu: false
            }
        );
    }

    #[test]
    fn quick_exit_on_cpu_override_gives_up() {
        assert_eq!(after_browser_exit(false, false, 2, SOON), ExitAction::Exit);
    }

    #[test]
    fn args_for_sets_hidden_flags() {
        let cur = s(&[
            "--url",
            "x",
            "--unexpected-exit-count",
            "1",
            "--wait-for-pid",
            "3",
        ]);
        assert_eq!(
            args_for(
                ExitAction::Relaunch {
                    unexpected_count: 2,
                    gpu_cpu: true
                },
                &cur,
                9
            ),
            s(&[
                "--url",
                "x",
                "--unexpected-exit-count",
                "2",
                "--gpu-override",
                "cpu",
                "--wait-for-pid",
                "9"
            ])
        );
    }

    #[test]
    fn restart_now_drops_hidden_flags() {
        // The saved setting applies again after "Restart now".
        let cur = s(&[
            "--gpu-override=cpu",
            "--unexpected-exit-count",
            "2",
            "--no-open",
        ]);
        assert_eq!(
            args_for(
                ExitAction::Relaunch {
                    unexpected_count: 0,
                    gpu_cpu: false
                },
                &cur,
                9
            ),
            s(&["--no-open", "--wait-for-pid", "9"])
        );
    }

    #[test]
    fn wait_for_pid_sees_exit() {
        let child = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "exit"])
                .spawn()
        } else {
            std::process::Command::new("true").spawn()
        };
        let mut child = child.unwrap();
        let pid = child.id();
        let _ = child.wait();
        assert!(wait_for_pid(pid, Duration::from_secs(2)));
    }

    #[test]
    fn wait_for_pid_times_out_on_a_live_process() {
        let me = std::process::id();
        let t = std::time::Instant::now();
        assert!(!wait_for_pid(me, Duration::from_millis(300)));
        assert!(t.elapsed() >= Duration::from_millis(250));
    }
}
