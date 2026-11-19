//! "Start with Windows": the value `Page2Cam` under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` is the only source of
//! truth. Functions take the subkey so tests use their own key.

use std::path::Path;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};

/// The real Run key (HKCU).
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Our value's name under it.
pub const VALUE_NAME: &str = "Page2Cam";

/// The Run value for `exe`: the quoted path (starts the tray, stopped).
pub fn run_value(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// Does `value` start `exe`? (Case-insensitive, quotes optional.)
pub fn is_value_for(value: &str, exe: &Path) -> bool {
    value
        .trim()
        .trim_matches('"')
        .eq_ignore_ascii_case(&exe.display().to_string())
}

/// Read the string value `name` under `HKCU\<subkey>`.
pub fn get(subkey: &str, name: &str) -> Option<String> {
    let (sub, name) = (HSTRING::from(subkey), HSTRING::from(name));
    let mut buf = [0u16; 2048];
    let mut bytes = std::mem::size_of_val(&buf) as u32;
    // SAFETY: `buf` is `bytes` long; the strings live for the call.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &sub,
            &name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let n = (bytes as usize / 2).min(buf.len());
    Some(
        String::from_utf16_lossy(&buf[..n])
            .trim_end_matches('\0')
            .to_string(),
    )
}

/// Set the string value `name` under `HKCU\<subkey>` (creating the key).
pub fn set(subkey: &str, name: &str, value: &str) -> anyhow::Result<()> {
    let sub = HSTRING::from(subkey);
    let mut key = HKEY::default();
    // SAFETY: out-pointers valid for the call.
    let r = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &sub,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    };
    anyhow::ensure!(r == ERROR_SUCCESS, "opening HKCU\\{subkey}: {r:?}");
    let data: Vec<u8> = value
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    // SAFETY: `key` is open; `data` is a NUL-terminated UTF-16 string.
    let r = unsafe {
        let r = RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(&data));
        let _ = RegCloseKey(key);
        r
    };
    anyhow::ensure!(r == ERROR_SUCCESS, "writing HKCU\\{subkey}\\{name}: {r:?}");
    Ok(())
}

/// Remove the value `name` under `HKCU\<subkey>` (missing is fine).
pub fn remove(subkey: &str, name: &str) -> anyhow::Result<()> {
    // SAFETY: the strings live for the call.
    let r = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey),
            &HSTRING::from(name),
        )
    };
    anyhow::ensure!(
        r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND,
        "removing HKCU\\{subkey}\\{name}: {r:?}"
    );
    Ok(())
}

/// Is "Start with Windows" on for this exe?
pub fn enabled(exe: &Path) -> bool {
    get(RUN_KEY, VALUE_NAME).is_some_and(|v| is_value_for(&v, exe))
}

/// Turn "Start with Windows" on (for this exe) or off.
pub fn set_enabled(exe: &Path, on: bool) -> anyhow::Result<()> {
    if on {
        set(RUN_KEY, VALUE_NAME, &run_value(exe))
    } else {
        remove(RUN_KEY, VALUE_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_key_value_roundtrip() {
        // A test key, never the real Run key.
        let parent = r"Software\Page2CamTest";
        let sub = format!(r"{parent}\{}", std::process::id());
        let exe = Path::new(r"C:\Program Files\Page2Cam\page2cam.exe");
        let v = run_value(exe);
        assert_eq!(v, r#""C:\Program Files\Page2Cam\page2cam.exe""#);
        assert!(is_value_for(&v, exe));
        assert!(is_value_for(r"c:\program files\page2cam\PAGE2CAM.EXE", exe));
        assert!(!is_value_for(r#""D:\Page2Cam\page2cam.exe""#, exe));

        assert_eq!(get(&sub, VALUE_NAME), None);
        set(&sub, VALUE_NAME, &v).unwrap();
        assert_eq!(get(&sub, VALUE_NAME).as_deref(), Some(v.as_str()));
        remove(&sub, VALUE_NAME).unwrap();
        assert_eq!(get(&sub, VALUE_NAME), None);
        remove(&sub, VALUE_NAME).unwrap(); // already gone: fine
        delete_test_key(&sub, parent);
    }

    fn delete_test_key(sub: &str, parent: &str) {
        use windows::core::HSTRING;
        use windows::Win32::System::Registry::{RegDeleteKeyW, HKEY_CURRENT_USER};
        unsafe {
            let _ = RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(sub));
            // Only succeeds when no other test run left a subkey there.
            let _ = RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(parent));
        }
    }
}
