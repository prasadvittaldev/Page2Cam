//! IMFActivate handed to Frame Server; it activates our media source.
use std::sync::Mutex;

use windows::core::{implement, Interface, Result, GUID};
use windows::Win32::Foundation::E_POINTER;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFActivate_Impl, IMFAttributes, IMFMediaSourceEx, MFT_TRANSFORM_CLSID_Attribute,
    MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES,
};

use super::attrs::{forward_attributes, new_store};

#[implement(IMFActivate, IMFAttributes)]
pub(crate) struct Activator {
    attrs: IMFAttributes,
    source: Mutex<Option<IMFMediaSourceEx>>,
}

forward_attributes!(Activator_Impl, attrs);

pub(crate) fn create() -> Result<IMFActivate> {
    let attrs = new_store()?;
    unsafe {
        attrs.SetUINT32(&MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES, 1)?;
        attrs.SetGUID(&MFT_TRANSFORM_CLSID_Attribute, &super::CLSID)?;
    }
    let source = super::source::create(&attrs)?;
    super::log("activator created");
    Ok(Activator {
        attrs,
        source: Mutex::new(Some(source)),
    }
    .into())
}

impl IMFActivate_Impl for Activator_Impl {
    fn ActivateObject(&self, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> Result<()> {
        if riid.is_null() || ppv.is_null() {
            return Err(E_POINTER.into());
        }
        let guard = self.source.lock().unwrap();
        let src = guard
            .as_ref()
            .ok_or(windows::Win32::Media::MediaFoundation::MF_E_SHUTDOWN)?;
        let r = unsafe { src.query(riid, ppv).ok() };
        if let Err(e) = &r {
            super::log(&format!("ActivateObject failed for {:?}: {e}", unsafe {
                *riid
            }));
        }
        r
    }

    fn ShutdownObject(&self) -> Result<()> {
        Ok(())
    }

    fn DetachObject(&self) -> Result<()> {
        *self.source.lock().unwrap() = None;
        Ok(())
    }
}
