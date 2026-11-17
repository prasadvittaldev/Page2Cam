//! Reader side of p2c-shm: the DLL creates the Global mapping (the Frame Server
//! service is allowed to; a normal user process is not) with a DACL that lets
//! the interactive user's page2cam.exe open it for writing.
use std::sync::OnceLock;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, FILE_MAP_ALL_ACCESS, FILE_MAP_READ,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};

struct Mapping {
    _handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

static MAPPING: OnceLock<Option<Mapping>> = OnceLock::new();

/// Everyone, SYSTEM and LocalService get full access; low-integrity writers allowed.
const SDDL: &str = "D:(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;LS)S:(ML;;NW;;;LW)";

fn open() -> Option<Mapping> {
    let name = HSTRING::from(p2c_shm::MAPPING_NAME);
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let sa = ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(SDDL),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .ok()
        .map(|_| SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        });
        let size = p2c_shm::MAPPING_SIZE as u64;
        let handle = match CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            sa.as_ref().map(|s| s as *const _),
            PAGE_READWRITE,
            (size >> 32) as u32,
            size as u32,
            PCWSTR(name.as_ptr()),
        ) {
            Ok(h) => h,
            Err(e) => {
                super::log(&format!(
                    "CreateFileMapping failed ({e}); trying to open an existing one"
                ));
                match OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(name.as_ptr())) {
                    Ok(h) => h,
                    Err(e) => {
                        super::log(&format!("OpenFileMapping failed: {e}"));
                        return None;
                    }
                }
            }
        };
        let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, p2c_shm::MAPPING_SIZE);
        let view = if view.Value.is_null() {
            MapViewOfFile(handle, FILE_MAP_READ, 0, 0, p2c_shm::MAPPING_SIZE)
        } else {
            view
        };
        if view.Value.is_null() {
            super::log("MapViewOfFile failed");
            return None;
        }
        super::log("frame mapping ready");
        Some(Mapping {
            _handle: handle,
            view,
        })
    }
}

/// Base pointer of the mapping, created on first use.
pub(crate) fn base() -> Option<*const u8> {
    MAPPING
        .get_or_init(open)
        .as_ref()
        .map(|m| m.view.Value as *const u8)
}
