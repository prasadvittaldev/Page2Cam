//! The engine child process: spawn (`--engine …`, token in the environment,
//! no window), put it in a kill-on-close job, report its exit, and poll its
//! `/api/state` every 2 s. Every report carries the spawn generation.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};

use super::control::Cmd;
use super::job::Job;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Health poll interval while the engine runs.
pub const POLL_EVERY: Duration = Duration::from_secs(2);
/// How long `kill` waits for the engine process to be gone.
const KILL_WAIT: Duration = Duration::from_secs(3);

pub struct Engine {
    pub gen: u64,
    pub port: u16,
    /// `None` if the job could not be made (then only the engine is killed;
    /// `StopLeftovers` catches its helpers).
    job: Option<Job>,
    process: HANDLE,
    /// Ends the poller.
    stop: Arc<AtomicBool>,
}

// SAFETY: the process handle is usable from any thread.
unsafe impl Send for Engine {}

/// Start the engine for generation `gen`; its exit and polls go to `tx`.
pub fn spawn(gen: u64, port: u16, token: &str, tx: Sender<Cmd>) -> anyhow::Result<Engine> {
    let cmd = crate::launch::engine_command(port, std::process::id(), token)
        .context("the admin token is not valid")?;
    let exe = std::env::current_exe().context("finding page2cam.exe")?;
    let mut child = Command::new(&exe)
        .args(&cmd.args)
        .envs(cmd.env.iter().map(|(k, v)| (k, v)))
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))?;
    let pid = child.id();
    let job = match Job::new().and_then(|j| j.assign(&child).map(|()| j)) {
        Ok(j) => Some(j),
        Err(e) => {
            tracing::warn!("engine runs without a job object: {e:#}");
            None
        }
    };
    // Valid: the pid can't be reused before `child` is waited for.
    // SAFETY: plain call.
    let process = match unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) }
    {
        Ok(h) => h,
        Err(e) => {
            let _ = child.kill();
            return Err(anyhow::Error::from(e).context("opening the engine process"));
        }
    };
    tracing::info!(gen, pid, port, args = ?cmd.args, "engine started");
    let stop = Arc::new(AtomicBool::new(false));
    // Owns the process handle and the job from here on: any early return
    // below closes both (and the job takes the engine down with it).
    let engine = Engine {
        gen,
        port,
        job,
        process,
        stop: stop.clone(),
    };
    {
        let stop = stop.clone();
        let tx = tx.clone();
        let waiter = std::thread::Builder::new()
            .name("p2c-tray-engine-wait".into())
            .spawn(move || {
                let code = match child.wait() {
                    Ok(s) => s.code().unwrap_or(1),
                    Err(e) => {
                        tracing::warn!("waiting for the engine: {e}");
                        1
                    }
                };
                stop.store(true, Ordering::Release);
                tracing::info!(gen, pid, code, "engine exited");
                let _ = tx.send(Cmd::EngineExited { gen, code });
            });
        if let Err(e) = waiter {
            engine.kill();
            return Err(anyhow::Error::from(e).context("starting the engine watcher"));
        }
    }
    {
        let stop = stop.clone();
        let token = token.to_string();
        let spawned = std::thread::Builder::new()
            .name("p2c-tray-poll".into())
            .spawn(move || loop {
                std::thread::sleep(POLL_EVERY);
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let result = crate::http::get_health(port, &token).map_err(|e| format!("{e:#}"));
                if stop.load(Ordering::Acquire) || tx.send(Cmd::Poll { gen, result }).is_err() {
                    return;
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("cannot poll the engine: {e}");
        }
    }
    Ok(engine)
}

impl Engine {
    /// End the engine tree without waiting (sign-out: Windows allows ~5 s).
    pub fn kill_now(&self) {
        tracing::info!(gen = self.gen, "ending the engine's processes (sign-out)");
        // SAFETY: handles we own.
        unsafe {
            match &self.job {
                Some(j) => j.kill(),
                None => {
                    let _ = TerminateProcess(self.process, 1);
                }
            }
        }
    }

    /// End the engine's whole process tree now; waits up to 3 s for it.
    pub fn kill(&self) {
        tracing::info!(gen = self.gen, "ending the engine's processes");
        // SAFETY: handles we own.
        unsafe {
            match &self.job {
                Some(j) => j.kill(),
                None => {
                    let _ = TerminateProcess(self.process, 1);
                }
            }
            WaitForSingleObject(self.process, KILL_WAIT.as_millis() as u32);
        }
    }
}

impl Drop for Engine {
    /// Closing the job ends anything still in it (helpers of an engine that
    /// already exited, or the engine itself).
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // SAFETY: we own the handle.
        unsafe {
            let _ = CloseHandle(self.process);
        }
    }
}
