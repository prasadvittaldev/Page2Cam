//! The tray menu's items for a given state (the Windows shell turns them
//! into a popup menu).

use crate::state::TrayState;

/// Menu commands (also the Win32 menu item ids).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum MenuId {
    Start = 1,
    Stop = 2,
    OpenPage = 3,
    ForceRestart = 4,
    Update = 5,
    Autostart = 6,
    Quit = 7,
    /// "Allow agents" (checkbox).
    Agents = 8,
    /// "Check for updates" (when no update is known yet).
    CheckUpdates = 9,
}

impl MenuId {
    pub fn from_u32(v: u32) -> Option<MenuId> {
        use MenuId::*;
        [
            Start,
            Stop,
            OpenPage,
            ForceRestart,
            Update,
            Autostart,
            Quit,
            Agents,
            CheckUpdates,
        ]
        .into_iter()
        .find(|m| *m as u32 == v)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Item {
        /// `None`: an information line (greyed, does nothing).
        id: Option<MenuId>,
        label: String,
        enabled: bool,
        checked: bool,
    },
    Separator,
}

fn item(id: MenuId, label: &str, enabled: bool) -> Entry {
    Entry::Item {
        id: Some(id),
        label: label.into(),
        enabled,
        checked: false,
    }
}

/// An update on offer: version, and whether this copy can install it
/// itself (installed) or only link to the download page (portable).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateOffer {
    pub version: String,
    pub can_install: bool,
}

/// The menu's version line and update item: the running version, the
/// newer release on offer (if any), and whether a check is running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateMenu {
    pub current: String,
    pub offer: Option<UpdateOffer>,
    pub checking: bool,
}

impl UpdateMenu {
    /// No update known, no check running.
    pub fn idle(current: &str) -> UpdateMenu {
        UpdateMenu {
            current: current.into(),
            offer: None,
            checking: false,
        }
    }

    pub fn offering(current: &str, offer: &UpdateOffer) -> UpdateMenu {
        UpdateMenu {
            offer: Some(offer.clone()),
            ..UpdateMenu::idle(current)
        }
    }
}

/// The menu for `state`. `status` is the first, informational line (the
/// tooltip text). `agents`: "Allow agents" checked or not (`None`: the agent
/// endpoint isn't available, so no item).
pub fn entries(
    state: &TrayState,
    status: &str,
    update: &UpdateMenu,
    autostart: bool,
    agents: Option<bool>,
) -> Vec<Entry> {
    let mut v = vec![
        Entry::Item {
            id: None,
            label: status.into(),
            enabled: false,
            checked: false,
        },
        Entry::Item {
            id: None,
            label: format!("Version {}", update.current),
            enabled: false,
            checked: false,
        },
        Entry::Separator,
    ];
    match state {
        TrayState::Stopped | TrayState::Error(_) => v.push(item(MenuId::Start, "Start", true)),
        TrayState::Starting { .. } | TrayState::Running => v.push(item(MenuId::Stop, "Stop", true)),
        TrayState::Stopping { .. } => v.push(item(MenuId::Stop, "Stopping…", false)),
    }
    v.push(item(
        MenuId::OpenPage,
        "Open control page",
        matches!(state, TrayState::Running),
    ));
    v.push(item(MenuId::ForceRestart, "Force restart", true));
    v.push(Entry::Separator);
    match (&update.offer, update.checking) {
        (Some(u), _) => {
            let label = if u.can_install {
                format!("Update now to {}", u.version)
            } else {
                format!("Download update {}…", u.version)
            };
            v.push(item(MenuId::Update, &label, true));
        }
        (None, true) => v.push(item(MenuId::CheckUpdates, "Checking for updates…", false)),
        (None, false) => v.push(item(MenuId::CheckUpdates, "Check for updates", true)),
    }
    v.push(Entry::Item {
        id: Some(MenuId::Autostart),
        label: "Start with Windows".into(),
        enabled: true,
        checked: autostart,
    });
    if let Some(on) = agents {
        v.push(Entry::Item {
            id: Some(MenuId::Agents),
            label: "Allow agents (Claude, Codex…)".into(),
            enabled: true,
            checked: on,
        });
    }
    v.push(Entry::Separator);
    v.push(item(MenuId::Quit, "Quit Page2Cam", true));
    v
}

/// The menu once Quit was chosen: only the status line and a greyed
/// "Quitting…" (Start, Stop and Force restart are no longer offered).
pub fn quitting(status: &str) -> Vec<Entry> {
    vec![
        Entry::Item {
            id: None,
            label: status.into(),
            enabled: false,
            checked: false,
        },
        Entry::Separator,
        item(MenuId::Quit, "Quitting…", false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn ids(e: &[Entry]) -> Vec<(MenuId, bool)> {
        e.iter()
            .filter_map(|x| match x {
                Entry::Item {
                    id: Some(id),
                    enabled,
                    ..
                } => Some((*id, *enabled)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn menu_follows_state() {
        use MenuId::*;
        let stopped = entries(
            &TrayState::Stopped,
            "Page2Cam — stopped",
            &UpdateMenu::idle("0.6.0"),
            false,
            None,
        );
        assert_eq!(
            ids(&stopped),
            vec![
                (Start, true),
                (OpenPage, false),
                (ForceRestart, true),
                (CheckUpdates, true),
                (Autostart, true),
                (Quit, true)
            ]
        );
        assert!(stopped
            .iter()
            .any(|x| matches!(x, Entry::Item { id: None, label, .. } if label == "Version 0.6.0")));
        let running = entries(
            &TrayState::Running,
            "x",
            &UpdateMenu::idle("0.6.0"),
            true,
            None,
        );
        assert!(ids(&running).contains(&(Stop, true)));
        assert!(ids(&running).contains(&(OpenPage, true)));
        assert!(running.iter().any(|e| matches!(
            e,
            Entry::Item {
                id: Some(Autostart),
                checked: true,
                ..
            }
        )));
        let stopping = entries(
            &TrayState::Stopping {
                since: Instant::now(),
            },
            "x",
            &UpdateMenu::idle("0.6.0"),
            false,
            None,
        );
        assert!(ids(&stopping).contains(&(Stop, false)));
        let err = entries(
            &TrayState::Error("boom".into()),
            "x",
            &UpdateMenu::idle("0.6.0"),
            false,
            None,
        );
        assert!(ids(&err).contains(&(Start, true)));
    }

    #[test]
    fn allow_agents_item_shows_its_state() {
        let checked = |e: &[Entry]| {
            e.iter().find_map(|x| match x {
                Entry::Item {
                    id: Some(MenuId::Agents),
                    checked,
                    enabled,
                    ..
                } => Some((*checked, *enabled)),
                _ => None,
            })
        };
        assert_eq!(
            checked(&entries(
                &TrayState::Stopped,
                "x",
                &UpdateMenu::idle("0.6.0"),
                false,
                Some(true)
            )),
            Some((true, true))
        );
        assert_eq!(
            checked(&entries(
                &TrayState::Running,
                "x",
                &UpdateMenu::idle("0.6.0"),
                false,
                Some(false)
            )),
            Some((false, true))
        );
        assert_eq!(
            checked(&entries(
                &TrayState::Running,
                "x",
                &UpdateMenu::idle("0.6.0"),
                false,
                None
            )),
            None
        );
        assert_eq!(MenuId::from_u32(8), Some(MenuId::Agents));
    }

    #[test]
    fn update_item_only_when_offered() {
        let offer = UpdateOffer {
            version: "0.4.0".into(),
            can_install: true,
        };
        let e = entries(
            &TrayState::Stopped,
            "x",
            &UpdateMenu::offering("0.3.0", &offer),
            false,
            None,
        );
        assert!(e
            .iter()
            .any(|x| matches!(x, Entry::Item { label, .. } if label == "Update now to 0.4.0")));
        let portable = UpdateOffer {
            can_install: false,
            ..offer
        };
        let e = entries(
            &TrayState::Stopped,
            "x",
            &UpdateMenu::offering("0.3.0", &portable),
            false,
            None,
        );
        assert!(e.iter().any(
            |x| matches!(x, Entry::Item { label, .. } if label.starts_with("Download update 0.4.0"))
        ));
        assert!(!ids(&entries(
            &TrayState::Stopped,
            "x",
            &UpdateMenu::idle("0.6.0"),
            false,
            None
        ))
        .iter()
        .any(|(id, _)| *id == MenuId::Update));
        assert_eq!(MenuId::from_u32(5), Some(MenuId::Update));
        assert_eq!(MenuId::from_u32(99), None);
    }

    #[test]
    fn check_for_updates_item_when_nothing_is_offered() {
        let find = |m: &UpdateMenu| {
            entries(&TrayState::Stopped, "x", m, false, None)
                .into_iter()
                .find_map(|x| match x {
                    Entry::Item {
                        id: Some(id @ (MenuId::CheckUpdates | MenuId::Update)),
                        label,
                        enabled,
                        ..
                    } => Some((id, label, enabled)),
                    _ => None,
                })
        };
        assert_eq!(
            find(&UpdateMenu::idle("0.6.0")),
            Some((MenuId::CheckUpdates, "Check for updates".into(), true))
        );
        let checking = UpdateMenu {
            checking: true,
            ..UpdateMenu::idle("0.6.0")
        };
        assert_eq!(
            find(&checking),
            Some((MenuId::CheckUpdates, "Checking for updates…".into(), false))
        );
        let offer = UpdateOffer {
            version: "0.7.0".into(),
            can_install: true,
        };
        assert_eq!(
            find(&UpdateMenu::offering("0.6.0", &offer)),
            Some((MenuId::Update, "Update now to 0.7.0".into(), true))
        );
        assert_eq!(MenuId::from_u32(9), Some(MenuId::CheckUpdates));
    }

    #[test]
    fn quitting_menu_offers_nothing() {
        let e = quitting("Page2Cam — stopping…");
        assert!(ids(&e).iter().all(|(_, enabled)| !enabled), "{e:?}");
        assert!(e
            .iter()
            .any(|x| matches!(x, Entry::Item { label, .. } if label == "Quitting…")));
    }
}
