//! Page2Cam tray: core logic (state machine, health classification, engine
//! launch). Platform-free; the Windows shell drives it with events and carries
//! out the returned actions.

pub mod health;
pub mod launch;
pub mod standby;
pub mod state;

pub use health::{classify, effective_level, tooltip, Hysteresis, Level, SysLoad};
pub use launch::{engine_command, pick_free_port, valid_token, EngineCommand};
pub use standby::{standby_nv12, StandbyKind};
pub use state::{Action, Event, Supervisor, TrayState};
