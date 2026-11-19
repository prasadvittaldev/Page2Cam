//! A kill-on-close Job Object for the engine's process tree (engine,
//! Chromium helpers, ffmpeg): one call ends them all, and if the tray itself
//! dies Windows closes the handle and ends the tree too.
//!
//! Processes the engine starts after it was assigned are in the job
//! automatically. The engine is assigned right after it is spawned, long
//! before it starts Chromium (which happens after its settings, leftover
//! check and lock), so nothing escapes in practice.

use std::os::windows::io::AsRawHandle;
use std::process::Child;

use anyhow::Context;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

pub struct Job(HANDLE);

// SAFETY: a job handle is usable from any thread.
unsafe impl Send for Job {}

impl Job {
    /// A new job whose processes all end when it is closed.
    pub fn new() -> anyhow::Result<Job> {
        // SAFETY: plain calls; the info struct lives for the call.
        unsafe {
            let h = CreateJobObjectW(None, windows::core::PCWSTR::null())
                .context("creating a job object")?;
            let job = Job(h);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of_val(&info) as u32,
            )
            .context("setting kill-on-close on the job object")?;
            Ok(job)
        }
    }

    /// Put `child` (and every process it starts from now on) in the job.
    pub fn assign(&self, child: &Child) -> anyhow::Result<()> {
        // SAFETY: the child's handle is valid while `child` lives.
        unsafe { AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle())) }
            .context("adding the engine to its job object")
    }

    /// End every process in the job (exit code 1).
    pub fn kill(&self) {
        // SAFETY: valid job handle.
        if let Err(e) = unsafe { TerminateJobObject(self.0, 1) } {
            tracing::warn!("could not end the engine's processes: {e}");
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Kill-on-close: anything still in the job ends here.
        // SAFETY: we own the handle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn alive(pid: u32) -> bool {
        p2c_proc::list_processes().iter().any(|p| p.pid == pid)
    }

    #[test]
    fn job_object_kills_children() {
        use std::os::windows::process::CommandExt;
        let job = Job::new().unwrap();
        let child = std::process::Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 >NUL"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn()
            .unwrap();
        job.assign(&child).unwrap();
        let cmd = child.id();
        // Wait for cmd's own child (ping.exe), started after the assignment.
        let begun = Instant::now();
        let ping = loop {
            if let Some(p) = p2c_proc::list_processes()
                .into_iter()
                .find(|p| p.parent == cmd && p.name.eq_ignore_ascii_case("ping.exe"))
            {
                break p.pid;
            }
            assert!(
                begun.elapsed() < Duration::from_secs(10),
                "ping never started"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        job.kill();
        let begun = Instant::now();
        while (alive(cmd) || alive(ping)) && begun.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(cmd), "cmd still running");
        assert!(!alive(ping), "ping (grandchild) still running");
        drop(child);
    }
}
