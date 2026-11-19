//! The tray's update checks (15 s after start, then every 6 h, if the
//! `check_updates` setting allows) and install flow, shared with the
//! standalone app through [`crate::update::Updater`]. The engine run by the
//! tray never checks.

use std::sync::Arc;

use crate::update::{is_installed, Updater};

/// Make the updater and start its background checks when enabled.
pub fn start() -> Arc<Updater> {
    let u = Arc::new(Updater::new(
        env!("CARGO_PKG_VERSION"),
        is_installed(),
        Box::new(p2c_update::fetch_latest),
    ));
    if crate::settings::check_updates() {
        u.spawn_checks();
    } else {
        tracing::info!("update checks are off (check_updates = false)");
    }
    u
}
