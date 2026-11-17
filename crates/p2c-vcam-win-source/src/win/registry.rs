//! regsvr32 support: HKLM\Software\Classes\CLSID\{...}\InprocServer32.
//! Frame Server reads HKLM only, so this needs an elevated regsvr32.
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

fn key_path() -> String {
    format!("Software\\Classes\\CLSID\\{}", p2c_shm::CLSID_STR)
}

fn set_sz(key: HKEY, name: Option<&str>, value: &str) -> windows::core::Result<()> {
    let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
    let name = name.map(HSTRING::from);
    let r = unsafe {
        RegSetValueExW(
            key,
            name.as_ref()
                .map(|n| PCWSTR(n.as_ptr()))
                .unwrap_or(PCWSTR::null()),
            None,
            REG_SZ,
            Some(bytes),
        )
    };
    if r == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(r.to_hresult().into())
    }
}

pub(crate) fn register() -> windows::core::Result<()> {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(Some(super::module().into()), &mut buf) } as usize;
    let dll = String::from_utf16_lossy(&buf[..n]);
    let path = HSTRING::from(format!("{}\\InprocServer32", key_path()));
    let mut key = HKEY::default();
    let r = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &path,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
    };
    if r != ERROR_SUCCESS {
        return Err(r.to_hresult().into());
    }
    let res = set_sz(key, None, &dll).and_then(|_| set_sz(key, Some("ThreadingModel"), "Both"));
    unsafe {
        let _ = RegCloseKey(key);
    }
    super::log(&format!("registered {dll}"));
    res
}

pub(crate) fn unregister() -> windows::core::Result<()> {
    let r = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(key_path())) };
    if r == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(r.to_hresult().into())
    }
}
