//! The tray's popup menu from [`crate::menu::entries`].

use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, PostMessageW, SetForegroundWindow,
    TrackPopupMenu, MENU_ITEM_FLAGS, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING,
    TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
};

use crate::menu::{Entry, MenuId};

/// Show the menu at `at` (else at the cursor) and return the chosen command.
/// Runs a modal loop: the window procedure is re-entered meanwhile, so the
/// caller must not hold any borrow of its state.
pub fn show(hwnd: HWND, entries: &[Entry], at: Option<POINT>) -> Option<MenuId> {
    // SAFETY: Win32 menu calls on a menu we create and destroy here.
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for e in entries {
            let r = match e {
                Entry::Separator => AppendMenuW(menu, MF_SEPARATOR, 0, None),
                Entry::Item {
                    id,
                    label,
                    enabled,
                    checked,
                } => {
                    let mut flags: MENU_ITEM_FLAGS = MF_STRING;
                    if !enabled || id.is_none() {
                        flags |= MF_GRAYED;
                    }
                    if *checked {
                        flags |= MF_CHECKED;
                    }
                    // Menus treat '&' as an accelerator marker.
                    let text = HSTRING::from(label.replace('&', "&&"));
                    AppendMenuW(menu, flags, id.map_or(0, |i| i as usize), &text)
                }
            };
            if let Err(e) = r {
                tracing::warn!("building the tray menu: {e}");
            }
        }
        let pt = at.unwrap_or_else(|| {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            p
        });
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        MenuId::from_u32(chosen.0 as u32)
    }
}
