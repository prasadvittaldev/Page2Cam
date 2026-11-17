//! Media Foundation objects here are also IMFAttributes stores. We forward every
//! IMFAttributes call to an inner store from MFCreateAttributes, through its
//! vtable (the raw ABI matches the *_Impl signatures one to one).

/// `impl IMFAttributes_Impl for $ty` forwarding to `self.$field: IMFAttributes`.
macro_rules! forward_attributes {
    ($ty:ty, $field:ident) => {
        impl windows::Win32::Media::MediaFoundation::IMFAttributes_Impl for $ty {
            fn GetItem(
                &self,
                k: *const windows::core::GUID,
                v: *mut windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetItem)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        v,
                    )
                    .ok()
                }
            }
            fn GetItemType(
                &self,
                k: *const windows::core::GUID,
            ) -> windows::core::Result<windows::Win32::Media::MediaFoundation::MF_ATTRIBUTE_TYPE>
            {
                let mut out = Default::default();
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetItemType)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        &mut out,
                    )
                    .map(|| out)
                }
            }
            fn CompareItem(
                &self,
                k: *const windows::core::GUID,
                v: *const windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
            ) -> windows::core::Result<windows::core::BOOL> {
                let mut out = Default::default();
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).CompareItem)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        v,
                        &mut out,
                    )
                    .map(|| out)
                }
            }
            fn Compare(
                &self,
                theirs: windows_core::Ref<windows::Win32::Media::MediaFoundation::IMFAttributes>,
                mt: windows::Win32::Media::MediaFoundation::MF_ATTRIBUTES_MATCH_TYPE,
            ) -> windows::core::Result<windows::core::BOOL> {
                let mut out = Default::default();
                let p = theirs
                    .as_ref()
                    .map(|t| windows::core::Interface::as_raw(t))
                    .unwrap_or(std::ptr::null_mut());
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).Compare)(
                        windows::core::Interface::as_raw(&self.$field),
                        p,
                        mt,
                        &mut out,
                    )
                    .map(|| out)
                }
            }
            fn GetUINT32(&self, k: *const windows::core::GUID) -> windows::core::Result<u32> {
                let mut out = 0;
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetUINT32)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        &mut out,
                    )
                    .map(|| out)
                }
            }
            fn GetUINT64(&self, k: *const windows::core::GUID) -> windows::core::Result<u64> {
                let mut out = 0;
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetUINT64)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        &mut out,
                    )
                    .map(|| out)
                }
            }
            fn GetDouble(&self, k: *const windows::core::GUID) -> windows::core::Result<f64> {
                let mut out = 0.0;
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetDouble)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        &mut out,
                    )
                    .map(|| out)
                }
            }
            fn GetGUID(
                &self,
                k: *const windows::core::GUID,
            ) -> windows::core::Result<windows::core::GUID> {
                let mut out = windows::core::GUID::zeroed();
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetGUID)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        &mut out,
                    )
                    .map(|| out)
                }
            }
            fn GetStringLength(&self, k: *const windows::core::GUID) -> windows::core::Result<u32> {
                let mut out = 0;
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetStringLength)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        &mut out,
                    )
                    .map(|| out)
                }
            }
            fn GetString(
                &self,
                k: *const windows::core::GUID,
                v: windows::core::PWSTR,
                n: u32,
                len: *mut u32,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetString)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        v,
                        n,
                        len,
                    )
                    .ok()
                }
            }
            fn GetAllocatedString(
                &self,
                k: *const windows::core::GUID,
                v: *mut windows::core::PWSTR,
                len: *mut u32,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetAllocatedString)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        v,
                        len,
                    )
                    .ok()
                }
            }
            fn GetBlobSize(&self, k: *const windows::core::GUID) -> windows::core::Result<u32> {
                let mut out = 0;
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetBlobSize)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        &mut out,
                    )
                    .map(|| out)
                }
            }
            fn GetBlob(
                &self,
                k: *const windows::core::GUID,
                b: *mut u8,
                n: u32,
                len: *mut u32,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetBlob)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        b,
                        n,
                        len,
                    )
                    .ok()
                }
            }
            fn GetAllocatedBlob(
                &self,
                k: *const windows::core::GUID,
                b: *mut *mut u8,
                len: *mut u32,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetAllocatedBlob)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        b,
                        len,
                    )
                    .ok()
                }
            }
            fn GetUnknown(
                &self,
                k: *const windows::core::GUID,
                riid: *const windows::core::GUID,
                ppv: *mut *mut core::ffi::c_void,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetUnknown)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        riid,
                        ppv,
                    )
                    .ok()
                }
            }
            fn SetItem(
                &self,
                k: *const windows::core::GUID,
                v: *const windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).SetItem)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        v,
                    )
                    .ok()
                }
            }
            fn DeleteItem(&self, k: *const windows::core::GUID) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).DeleteItem)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                    )
                    .ok()
                }
            }
            fn DeleteAllItems(&self) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).DeleteAllItems)(
                        windows::core::Interface::as_raw(&self.$field),
                    )
                    .ok()
                }
            }
            fn SetUINT32(
                &self,
                k: *const windows::core::GUID,
                v: u32,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).SetUINT32)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        v,
                    )
                    .ok()
                }
            }
            fn SetUINT64(
                &self,
                k: *const windows::core::GUID,
                v: u64,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).SetUINT64)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        v,
                    )
                    .ok()
                }
            }
            fn SetDouble(
                &self,
                k: *const windows::core::GUID,
                v: f64,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).SetDouble)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        v,
                    )
                    .ok()
                }
            }
            fn SetGUID(
                &self,
                k: *const windows::core::GUID,
                v: *const windows::core::GUID,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).SetGUID)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        v,
                    )
                    .ok()
                }
            }
            fn SetString(
                &self,
                k: *const windows::core::GUID,
                v: &windows::core::PCWSTR,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).SetString)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        *v,
                    )
                    .ok()
                }
            }
            fn SetBlob(
                &self,
                k: *const windows::core::GUID,
                b: *const u8,
                n: u32,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).SetBlob)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        b,
                        n,
                    )
                    .ok()
                }
            }
            fn SetUnknown(
                &self,
                k: *const windows::core::GUID,
                u: windows_core::Ref<windows::core::IUnknown>,
            ) -> windows::core::Result<()> {
                let p = u
                    .as_ref()
                    .map(|t| windows::core::Interface::as_raw(t))
                    .unwrap_or(std::ptr::null_mut());
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).SetUnknown)(
                        windows::core::Interface::as_raw(&self.$field),
                        k,
                        p,
                    )
                    .ok()
                }
            }
            fn LockStore(&self) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).LockStore)(
                        windows::core::Interface::as_raw(&self.$field),
                    )
                    .ok()
                }
            }
            fn UnlockStore(&self) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).UnlockStore)(
                        windows::core::Interface::as_raw(&self.$field),
                    )
                    .ok()
                }
            }
            fn GetCount(&self) -> windows::core::Result<u32> {
                let mut out = 0;
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetCount)(
                        windows::core::Interface::as_raw(&self.$field),
                        &mut out,
                    )
                    .map(|| out)
                }
            }
            fn GetItemByIndex(
                &self,
                i: u32,
                k: *mut windows::core::GUID,
                v: *mut windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
            ) -> windows::core::Result<()> {
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).GetItemByIndex)(
                        windows::core::Interface::as_raw(&self.$field),
                        i,
                        k,
                        v,
                    )
                    .ok()
                }
            }
            fn CopyAllItems(
                &self,
                dest: windows_core::Ref<windows::Win32::Media::MediaFoundation::IMFAttributes>,
            ) -> windows::core::Result<()> {
                let p = dest
                    .as_ref()
                    .map(|t| windows::core::Interface::as_raw(t))
                    .unwrap_or(std::ptr::null_mut());
                unsafe {
                    (windows::core::Interface::vtable(&self.$field).CopyAllItems)(
                        windows::core::Interface::as_raw(&self.$field),
                        p,
                    )
                    .ok()
                }
            }
        }
    };
}
pub(crate) use forward_attributes;

/// A fresh attribute store.
pub(crate) fn new_store(
) -> windows::core::Result<windows::Win32::Media::MediaFoundation::IMFAttributes> {
    let mut a = None;
    unsafe { windows::Win32::Media::MediaFoundation::MFCreateAttributes(&mut a, 8)? };
    a.ok_or_else(|| windows::Win32::Foundation::E_OUTOFMEMORY.into())
}
