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
}

impl MenuId {
    pub fn from_u32(v: u32) -> Option<MenuId> {
        use MenuId::*;
        [Start, Stop, OpenPage, ForceRestart, Update, Autostart, Quit]
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

/// The menu for `state`. `status` is the first, informational line (the
/// tooltip text).
pub fn entries(
    state: &TrayState,
    status: &str,
    update: Option<&UpdateOffer>,
    autostart: bool,
) -> Vec<Entry> {
    let mut v = vec![
        Entry::Item {
            id: None,
            label: status.into(),
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
    if let Some(u) = update {
        let label = if u.can_install {
            format!("Install update {}", u.version)
        } else {
            format!("Download update {}…", u.version)
        };
        v.push(item(MenuId::Update, &label, true));
    }
    v.push(Entry::Item {
        id: Some(MenuId::Autostart),
        label: "Start with Windows".into(),
        enabled: true,
        checked: autostart,
    });
    v.push(Entry::Separator);
    v.push(item(MenuId::Quit, "Quit Page2Cam", true));
    v
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
        let stopped = entries(&TrayState::Stopped, "Page2Cam — stopped", None, false);
        assert_eq!(
            ids(&stopped),
            vec![
                (Start, true),
                (OpenPage, false),
                (ForceRestart, true),
                (Autostart, true),
                (Quit, true)
            ]
        );
        let running = entries(&TrayState::Running, "x", None, true);
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
            None,
            false,
        );
        assert!(ids(&stopping).contains(&(Stop, false)));
        let err = entries(&TrayState::Error("boom".into()), "x", None, false);
        assert!(ids(&err).contains(&(Start, true)));
    }

    #[test]
    fn update_item_only_when_offered() {
        let offer = UpdateOffer {
            version: "0.4.0".into(),
            can_install: true,
        };
        let e = entries(&TrayState::Stopped, "x", Some(&offer), false);
        assert!(e
            .iter()
            .any(|x| matches!(x, Entry::Item { label, .. } if label == "Install update 0.4.0")));
        let portable = UpdateOffer {
            can_install: false,
            ..offer
        };
        let e = entries(&TrayState::Stopped, "x", Some(&portable), false);
        assert!(e.iter().any(
            |x| matches!(x, Entry::Item { label, .. } if label.starts_with("Download update 0.4.0"))
        ));
        assert!(!ids(&entries(&TrayState::Stopped, "x", None, false))
            .iter()
            .any(|(id, _)| *id == MenuId::Update));
        assert_eq!(MenuId::from_u32(5), Some(MenuId::Update));
        assert_eq!(MenuId::from_u32(99), None);
    }
}
