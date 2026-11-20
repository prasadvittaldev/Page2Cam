//! Page2Cam tray: core logic (state machine, health classification, engine
//! launch, menu, icon pictures, admin client) is platform-free; the Windows
//! shell under `win/` drives it with events and carries out the returned
//! actions.

pub mod agent_backend;
pub mod agent_server;
pub mod agents;
pub mod health;
pub mod http;
pub mod icon;
pub mod launch;
pub mod menu;
pub mod notify;
pub mod settings;
pub mod single;
pub mod standby;
pub mod state;
pub mod update;

#[cfg(windows)]
mod win;

pub use health::{classify, effective_level, tooltip, Hysteresis, Level, SysLoad};
pub use launch::{engine_command, pick_free_port, valid_token, EngineCommand};
pub use standby::{standby_nv12, StandbyKind};
pub use state::{exit_handling, Action, Event, ExitHandling, Supervisor, TrayState};

/// Run the Windows tray until the user quits it. A second launch only asks
/// the running tray to show itself and returns `Ok(())`.
#[cfg(windows)]
pub fn run() -> anyhow::Result<()> {
    win::run()
}

/// Is a Page2Cam tray running in this session (`Local\Page2Cam.Tray` held)?
pub fn tray_running() -> bool {
    p2c_proc::lock_held(p2c_proc::TRAY_LOCK)
}
