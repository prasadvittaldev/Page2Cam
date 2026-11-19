//! Stop leftover Page2Cam processes at start.
//!
//! A previous instance (or its orphaned Chromium helpers) can keep the
//! virtual camera, the admin port or the CEF profile locked. Before opening
//! any of them we stop every other Page2Cam process, except ourselves, our
//! own helpers (descendants) and our ancestors (e.g. a tray process that
//! started the engine).

use std::path::PathBuf;
use std::time::Duration;

/// One running process, as seen by the platform scan.
#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
    pub path: Option<PathBuf>,
}

/// Maximum parent-chain steps followed (guards against pid-reuse cycles).
const MAX_CHAIN: usize = 64;

const WAIT_EXIT: Duration = Duration::from_secs(3);

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
/// descendants of `me` (our CEF helpers) and ancestors of `me`.
pub fn to_stop(procs: &[Proc], me: u32) -> Vec<u32> {
    let keep_up = ancestors(procs, me);
    procs
        .iter()
        .filter(|p| is_page2cam(&p.name))
        .filter(|p| p.pid != me && !keep_up.contains(&p.pid))
        .filter(|p| !ancestors(procs, p.pid).contains(&me))
        .map(|p| p.pid)
        .collect()
}

/// Stop all other Page2Cam processes; returns the pids that were targeted.
pub fn stop_others(me: u32) -> Vec<u32> {
    let procs = platform::list();
    let pids = to_stop(&procs, me);
    for pid in &pids {
        let path = procs
            .iter()
            .find(|p| p.pid == *pid)
            .and_then(|p| p.path.clone());
        tracing::warn!(pid, path = ?path, "stopped a leftover Page2Cam process");
    }
    platform::stop(&pids, WAIT_EXIT);
    pids
}

#[cfg(windows)]
mod platform {
    use super::Proc;
    use std::path::PathBuf;
    use std::time::Duration;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE,
    };

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
                if name.eq_ignore_ascii_case("page2cam.exe") {
                    if let Ok(h) =
                        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, e.th32ProcessID)
                    {
                        path = image_path(h);
                        let _ = CloseHandle(h);
                    }
                }
                out.push(Proc {
                    pid: e.th32ProcessID,
                    parent: e.th32ParentProcessID,
                    name,
                    path,
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
            let parent = stat[r + 1..]
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let path: Option<PathBuf> = std::fs::read_link(entry.path().join("exe")).ok();
            out.push(Proc {
                pid,
                parent,
                name,
                path,
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
        }
    }

    #[test]
    fn stops_other_main_instance() {
        let procs = [p(1, 0, "explorer"), p(10, 1, exe()), p(20, 1, exe())];
        assert_eq!(to_stop(&procs, 20), vec![10]);
    }

    #[test]
    fn keeps_own_helpers() {
        let procs = [p(20, 1, exe()), p(21, 20, exe()), p(22, 21, exe())];
        assert!(to_stop(&procs, 20).is_empty());
    }

    #[test]
    fn keeps_own_ancestors() {
        let procs = [p(5, 1, exe()), p(6, 5, exe()), p(20, 6, exe())];
        assert!(to_stop(&procs, 20).is_empty());
    }

    #[test]
    fn survives_parent_cycles() {
        let procs = [p(5, 6, exe()), p(6, 5, exe()), p(20, 5, exe())];
        // Must terminate; the cycle members are our ancestors, so kept.
        assert!(to_stop(&procs, 20).is_empty());
    }

    #[test]
    fn stops_orphaned_helpers_of_dead_instance() {
        let procs = [p(20, 1, exe()), p(30, 999, exe())];
        assert_eq!(to_stop(&procs, 20), vec![30]);
    }

    #[test]
    fn ignores_other_programs() {
        let procs = [
            p(20, 1, exe()),
            p(30, 1, "chrome.exe"),
            p(31, 1, "page2cam-tool"),
        ];
        assert!(to_stop(&procs, 20).is_empty());
    }

    #[test]
    #[cfg(windows)]
    fn name_match_is_case_insensitive_on_windows() {
        let procs = [p(20, 1, "page2cam.exe"), p(30, 1, "Page2Cam.EXE")];
        assert_eq!(to_stop(&procs, 20), vec![30]);
    }
}
