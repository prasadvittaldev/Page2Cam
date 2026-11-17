mod activator;
mod attrs;
mod log;
mod registry;
mod shared;
mod source;
mod stream;

use std::sync::atomic::{AtomicPtr, Ordering};

use windows::core::{implement, IUnknown, Interface, BOOL, GUID, HRESULT};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_POINTER, HINSTANCE, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

pub(crate) use log::log;

pub(crate) const CLSID: GUID = GUID::from_u128(0x7c2f9a41_3b6e_4d8a_9c1f_5e2b8d4a6f10);

static MODULE: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

pub(crate) fn module() -> HINSTANCE {
    HINSTANCE(MODULE.load(Ordering::Relaxed))
}

#[no_mangle]
extern "system" fn DllMain(
    hinst: HINSTANCE,
    reason: u32,
    _reserved: *mut core::ffi::c_void,
) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(hinst.0, Ordering::Relaxed);
    }
    BOOL(1)
}

#[implement(IClassFactory)]
struct ClassFactory;

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        outer: windows_core::Ref<IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if ppv.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *ppv = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let activator =
            activator::create().inspect_err(|e| log(&format!("CreateInstance failed: {e}")))?;
        unsafe { activator.query(riid, ppv).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

#[no_mangle]
unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut core::ffi::c_void,
) -> HRESULT {
    if clsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    *ppv = std::ptr::null_mut();
    if *clsid != CLSID {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = ClassFactory.into();
    factory.query(riid, ppv)
}

/// Keep the DLL loaded for the life of the Frame Server process (simplest safe choice).
#[no_mangle]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

#[no_mangle]
extern "system" fn DllRegisterServer() -> HRESULT {
    match registry::register() {
        Ok(()) => S_OK,
        Err(e) => {
            log(&format!("DllRegisterServer failed: {e}"));
            e.code()
        }
    }
}

#[no_mangle]
extern "system" fn DllUnregisterServer() -> HRESULT {
    match registry::unregister() {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}
