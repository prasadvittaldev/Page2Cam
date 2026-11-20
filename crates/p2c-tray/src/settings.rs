//! The tray's own small state file (`tray.toml`: `tray_seen`) and a
//! read-only look at the engine's settings (`check_updates`). The tray never
//! writes the engine's settings file.

use std::path::{Path, PathBuf};

/// `<config dir>/page2cam` (the same folder as the engine's `config.toml`).
fn user_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "page2cam").map(|d| d.config_dir().to_path_buf())
}

fn table(text: &str) -> toml::Table {
    text.parse::<toml::Table>().unwrap_or_default()
}

/// `tray_seen = true` in this tray.toml text?
pub fn tray_seen_in(text: &str) -> bool {
    table(text)
        .get("tray_seen")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
}

/// `text` with `tray_seen = true` (other keys kept).
pub fn with_tray_seen(text: &str) -> String {
    let mut t = table(text);
    t.insert("tray_seen".into(), toml::Value::Boolean(true));
    toml::to_string(&t).unwrap_or_else(|_| "tray_seen = true\n".into())
}

/// `agents_allowed = true` in this tray.toml text? (Default off.)
pub fn agents_allowed_in(text: &str) -> bool {
    table(text)
        .get("agents_allowed")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
}

/// `text` with `agents_allowed` set (other keys kept).
pub fn with_agents_allowed(text: &str, on: bool) -> String {
    let mut t = table(text);
    t.insert("agents_allowed".into(), toml::Value::Boolean(on));
    toml::to_string(&t).unwrap_or_default()
}

/// The saved agent token, if it looks like one of ours (32+ hex chars).
pub fn agent_token_in(text: &str) -> Option<String> {
    table(text)
        .get("agent_token")
        .and_then(toml::Value::as_str)
        .filter(|t| t.len() >= 32 && t.chars().all(|c| c.is_ascii_hexdigit()))
        .map(String::from)
}

/// `text` with `agent_token` set (other keys kept).
pub fn with_agent_token(text: &str, token: &str) -> String {
    let mut t = table(text);
    t.insert("agent_token".into(), toml::Value::String(token.into()));
    toml::to_string(&t).unwrap_or_default()
}

/// The tray's `tray.toml` path.
pub fn tray_toml_path() -> Option<PathBuf> {
    user_dir().map(|d| d.join("tray.toml"))
}

/// tray.toml's text ("" when missing).
pub fn read_tray_toml() -> String {
    tray_toml_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

/// Replace tray.toml's text.
pub fn write_tray_toml(text: &str) -> anyhow::Result<()> {
    let path = tray_toml_path().ok_or_else(|| anyhow::anyhow!("no config directory"))?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::agents::write_atomic(&path, text.as_bytes())?;
    Ok(())
}

/// The `check_updates` setting in this settings text (default on).
pub fn check_updates_in(text: &str) -> bool {
    table(text)
        .get("check_updates")
        .and_then(toml::Value::as_bool)
        .unwrap_or(true)
}

/// The engine's settings file, found as the engine finds it (without
/// `--config`): `page2cam.toml` next to the exe, else the user's config.toml.
fn engine_settings(exe_dir: Option<&Path>, user_dir: Option<&Path>) -> Option<PathBuf> {
    exe_dir
        .map(|d| d.join("page2cam.toml"))
        .filter(|p| p.is_file())
        .or_else(|| {
            user_dir
                .map(|d| d.join("config.toml"))
                .filter(|p| p.is_file())
        })
}

/// Should the tray check for updates? (Missing or unreadable file: yes.)
pub fn check_updates() -> bool {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    engine_settings(exe_dir.as_deref(), user_dir().as_deref())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_none_or(|t| check_updates_in(&t))
}

/// The installer's sign-in choice from `autostart.default` (text `1` or `0`);
/// anything else (or no file, as in the portable zip) is "no choice".
pub fn parse_autostart_default(text: &str) -> Option<bool> {
    match text.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// What to do with the HKCU Run value on this start: `Some(on)` only on the
/// tray's first start with an installer choice; afterwards the Run key is the
/// only source of truth and this never touches it.
pub fn first_run_autostart(first_run: bool, installer_choice: Option<bool>) -> Option<bool> {
    if first_run {
        installer_choice
    } else {
        None
    }
}

/// The installer's choice, read from `autostart.default` next to the exe.
pub fn installer_autostart_default() -> Option<bool> {
    let exe = std::env::current_exe().ok()?;
    let text = std::fs::read_to_string(exe.parent()?.join("autostart.default")).ok()?;
    parse_autostart_default(&text)
}

/// Was the first-run balloon shown already?
pub fn tray_seen() -> bool {
    user_dir()
        .and_then(|d| std::fs::read_to_string(d.join("tray.toml")).ok())
        .is_some_and(|t| tray_seen_in(&t))
}

/// Remember that the first-run balloon was shown.
pub fn set_tray_seen() -> anyhow::Result<()> {
    let dir = user_dir().ok_or_else(|| anyhow::anyhow!("no config directory"))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("tray.toml");
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(&path, with_tray_seen(&old))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_seen_roundtrip() {
        assert!(!tray_seen_in(""));
        assert!(!tray_seen_in("not toml ["));
        let t = with_tray_seen("other = 3\n");
        assert!(tray_seen_in(&t));
        assert!(t.contains("other = 3"), "{t}");
        assert!(tray_seen_in(&with_tray_seen("tray_seen = false")));
    }

    #[test]
    fn agents_settings_roundtrip_keep_other_keys() {
        assert!(!agents_allowed_in(""));
        assert!(!agents_allowed_in("broken ["));
        let t = with_agents_allowed("tray_seen = true\n", true);
        assert!(agents_allowed_in(&t) && tray_seen_in(&t), "{t}");
        let tok = "0123456789abcdef0123456789abcdef";
        let t = with_agent_token(&t, tok);
        assert_eq!(agent_token_in(&t).as_deref(), Some(tok));
        assert!(agents_allowed_in(&t) && tray_seen_in(&t), "{t}");
        let t = with_agents_allowed(&t, false);
        assert!(!agents_allowed_in(&t));
        assert_eq!(agent_token_in(&t).as_deref(), Some(tok));
        assert_eq!(agent_token_in("agent_token = \"short\""), None);
        assert_eq!(agent_token_in("agent_token = 5"), None);
        // tray_seen keeps the agent keys too.
        let t = with_tray_seen(&t);
        assert_eq!(agent_token_in(&t).as_deref(), Some(tok));
    }

    #[test]
    fn first_run_applies_the_installer_choice_once() {
        assert_eq!(first_run_autostart(true, Some(true)), Some(true));
        assert_eq!(first_run_autostart(true, Some(false)), Some(false));
        assert_eq!(first_run_autostart(true, None), None);
        assert_eq!(first_run_autostart(false, Some(true)), None);
        assert_eq!(first_run_autostart(false, Some(false)), None);
        assert_eq!(parse_autostart_default("1\r\n"), Some(true));
        assert_eq!(parse_autostart_default("0"), Some(false));
        assert_eq!(parse_autostart_default("yes"), None);
        assert_eq!(parse_autostart_default(""), None);
    }

    #[test]
    fn check_updates_defaults_on() {
        assert!(check_updates_in(""));
        assert!(check_updates_in("url = \"x\""));
        assert!(!check_updates_in("check_updates = false"));
        assert!(check_updates_in("check_updates = true"));
    }

    #[test]
    fn engine_settings_prefers_the_file_next_to_the_exe() {
        let exe = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        assert_eq!(engine_settings(Some(exe.path()), Some(user.path())), None);
        std::fs::write(user.path().join("config.toml"), "").unwrap();
        assert_eq!(
            engine_settings(Some(exe.path()), Some(user.path())),
            Some(user.path().join("config.toml"))
        );
        std::fs::write(exe.path().join("page2cam.toml"), "").unwrap();
        assert_eq!(
            engine_settings(Some(exe.path()), Some(user.path())),
            Some(exe.path().join("page2cam.toml"))
        );
    }
}
