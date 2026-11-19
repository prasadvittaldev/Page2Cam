//! One tray per user session: the named lock `Local\Page2Cam.Tray`
//! ([`p2c_proc::TRAY_LOCK`]). A second launch asks the running tray to show
//! itself and exits. The engine's lock (`Local\Page2Cam.Engine`) is never
//! taken here.

/// Result of [`claim_single_instance`].
pub enum Instance {
    /// We are the tray. Keep the lock for the life of the process (`None`:
    /// the lock could not be created; we run without it).
    First(Option<p2c_proc::Lock>),
    /// Another tray holds the lock; it was signalled. Exit.
    Other,
}

/// Take the lock `name`; when another process holds it, call `signal`
/// (e.g. post "show" to the running tray) and return [`Instance::Other`].
pub fn claim_single_instance(name: &str, signal: impl FnOnce()) -> Instance {
    match p2c_proc::try_lock(name) {
        Ok(Some(lock)) => Instance::First(Some(lock)),
        Ok(None) => {
            signal();
            Instance::Other
        }
        Err(e) => {
            tracing::warn!("cannot create the tray lock, running without it: {e:#}");
            Instance::First(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn second_instance_signals_and_exits() {
        // Own name, so a running Page2Cam tray doesn't interfere.
        let name = format!("Page2Cam.Test.Tray.{}", std::process::id());
        let signals = Cell::new(0);
        let first = claim_single_instance(&name, || signals.set(signals.get() + 1));
        let Instance::First(Some(lock)) = first else {
            panic!("first claim must own the lock");
        };
        assert_eq!(signals.get(), 0, "the first tray signals nobody");
        assert!(matches!(
            claim_single_instance(&name, || signals.set(signals.get() + 1)),
            Instance::Other
        ));
        assert_eq!(signals.get(), 1, "the second launch signals the first");
        drop(lock);
        assert!(matches!(
            claim_single_instance(&name, || signals.set(signals.get() + 1)),
            Instance::First(Some(_))
        ));
        assert_eq!(signals.get(), 1);
    }
}
