//! IMFMediaSourceEx: one video stream, live.
use std::sync::Mutex;

use windows::core::{implement, w, ComObject, IUnknown, Interface, Result, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG, E_POINTER, S_OK};
use windows::Win32::Media::KernelStreaming::{
    IKsControl, IKsControl_Impl, KSCAMERAPROFILE_Legacy, KSIDENTIFIER,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;

use super::attrs::{forward_attributes, new_store};
use super::stream::{self, MediaStream};

/// HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND): "no such KS property" (what VCamSample returns).
pub(crate) const KS_NOT_FOUND: HRESULT = HRESULT(0x8007_0492_u32 as i32);

#[implement(
    IMFMediaSourceEx,
    IMFGetService,
    IKsControl,
    IMFSampleAllocatorControl,
    IMFAttributes
)]
pub(crate) struct MediaSource {
    attrs: IMFAttributes,
    st: Mutex<State>,
}

struct State {
    queue: Option<IMFMediaEventQueue>,
    descriptor: Option<IMFPresentationDescriptor>,
    stream: Option<ComObject<MediaStream>>,
}

forward_attributes!(MediaSource_Impl, attrs);

pub(crate) fn create(activator_attrs: &IMFAttributes) -> Result<IMFMediaSourceEx> {
    let attrs = new_store()?;
    unsafe {
        activator_attrs.CopyAllItems(&attrs)?;
        let collection = MFCreateSensorProfileCollection()?;
        let legacy = MFCreateSensorProfile(&KSCAMERAPROFILE_Legacy, 0, PCWSTR::null())?;
        legacy.AddProfileFilter(0, w!("((RES==;FRT<=30,1;SUT==))"))?;
        collection.AddProfile(&legacy)?;
        attrs.SetUnknown(&MF_DEVICEMFT_SENSORPROFILE_COLLECTION, &collection)?;
    }
    let stream = stream::create(0)?;
    let desc = stream.descriptor()?;
    let descriptor = unsafe { MFCreatePresentationDescriptor(Some(&[Some(desc)]))? };
    let queue = unsafe { MFCreateEventQueue()? };
    let obj = ComObject::new(MediaSource {
        attrs,
        st: Mutex::new(State {
            queue: Some(queue),
            descriptor: Some(descriptor),
            stream: Some(stream.clone()),
        }),
    });
    let ex: IMFMediaSourceEx = obj.to_interface();
    stream.set_source(ex.cast()?);
    Ok(ex)
}

impl MediaSource_Impl {
    fn queue(&self) -> Result<IMFMediaEventQueue> {
        self.st
            .lock()
            .unwrap()
            .queue
            .clone()
            .ok_or(MF_E_SHUTDOWN.into())
    }

    fn stream(&self) -> Result<ComObject<MediaStream>> {
        self.st
            .lock()
            .unwrap()
            .stream
            .clone()
            .ok_or(MF_E_SHUTDOWN.into())
    }
}

impl IMFMediaEventGenerator_Impl for MediaSource_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        let q = self.queue()?; // don't hold our lock while blocking
        unsafe { q.GetEvent(flags.0) }
    }
    fn BeginGetEvent(
        &self,
        cb: windows_core::Ref<IMFAsyncCallback>,
        state: windows_core::Ref<IUnknown>,
    ) -> Result<()> {
        unsafe { self.queue()?.BeginGetEvent(cb.as_ref(), state.as_ref()) }
    }
    fn EndGetEvent(&self, result: windows_core::Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.queue()?.EndGetEvent(result.as_ref()) }
    }
    fn QueueEvent(
        &self,
        met: u32,
        ext: *const GUID,
        hr: HRESULT,
        v: *const PROPVARIANT,
    ) -> Result<()> {
        unsafe { self.queue()?.QueueEventParamVar(met, ext, hr, v) }
    }
}

impl IMFMediaSource_Impl for MediaSource_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        let d = self
            .st
            .lock()
            .unwrap()
            .descriptor
            .clone()
            .ok_or(MF_E_SHUTDOWN)?;
        unsafe { d.Clone() }
    }

    fn Start(
        &self,
        pd: windows_core::Ref<IMFPresentationDescriptor>,
        time_format: *const GUID,
        start: *const PROPVARIANT,
    ) -> Result<()> {
        let pd = pd.as_ref().ok_or(E_POINTER)?;
        if start.is_null() {
            return Err(E_POINTER.into());
        }
        if !time_format.is_null() && unsafe { *time_format } != GUID::zeroed() {
            return Err(E_INVALIDARG.into());
        }
        let (queue, ours, stream) = {
            let st = self.st.lock().unwrap();
            (
                st.queue.clone().ok_or(MF_E_SHUTDOWN)?,
                st.descriptor.clone().ok_or(MF_E_SHUTDOWN)?,
                st.stream.clone().ok_or(MF_E_SHUTDOWN)?,
            )
        };
        unsafe {
            let count = pd.GetStreamDescriptorCount()?;
            if count != 1 {
                return Err(E_INVALIDARG.into());
            }
            let mut selected = false.into();
            let mut desc = None;
            pd.GetStreamDescriptorByIndex(0, &mut selected, &mut desc)?;
            let desc = desc.ok_or(E_FAIL)?;
            if desc.GetStreamIdentifier()? != 0 {
                return Err(E_FAIL.into());
            }
            let running = stream.state() != MF_STREAM_STATE_STOPPED;
            if selected.as_bool() && !running {
                ours.SelectStream(0)?;
                queue.QueueEventParamUnk(
                    MENewStream.0 as u32,
                    &GUID::zeroed(),
                    S_OK,
                    &stream.to_interface::<IUnknown>(),
                )?;
                let ty = desc.GetMediaTypeHandler()?.GetCurrentMediaType()?;
                stream.start(Some(ty))?;
            } else if !selected.as_bool() && running {
                ours.DeselectStream(0)?;
                stream.stop()?;
            }
            let time = PROPVARIANT::from(MFGetSystemTime());
            queue.QueueEventParamVar(MESourceStarted.0 as u32, &GUID::zeroed(), S_OK, &time)?;
        }
        super::log("source started");
        Ok(())
    }

    fn Stop(&self) -> Result<()> {
        let (queue, ours, stream) = {
            let st = self.st.lock().unwrap();
            (
                st.queue.clone().ok_or(MF_E_SHUTDOWN)?,
                st.descriptor.clone().ok_or(MF_E_SHUTDOWN)?,
                st.stream.clone().ok_or(MF_E_SHUTDOWN)?,
            )
        };
        stream.stop()?;
        unsafe {
            ours.DeselectStream(0)?;
            let time = PROPVARIANT::from(MFGetSystemTime());
            queue.QueueEventParamVar(MESourceStopped.0 as u32, &GUID::zeroed(), S_OK, &time)?;
        }
        super::log("source stopped");
        Ok(())
    }

    fn Pause(&self) -> Result<()> {
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> Result<()> {
        let mut st = self.st.lock().unwrap();
        let q = st.queue.take().ok_or(MF_E_SHUTDOWN)?;
        unsafe {
            let _ = q.Shutdown();
        }
        if let Some(s) = st.stream.take() {
            s.shutdown();
        }
        st.descriptor = None;
        super::log("source shut down");
        Ok(())
    }
}

impl IMFMediaSourceEx_Impl for MediaSource_Impl {
    fn GetSourceAttributes(&self) -> Result<IMFAttributes> {
        Ok(windows_core::IUnknownImpl::to_interface::<IMFAttributes>(
            self,
        ))
    }

    fn GetStreamAttributes(&self, id: u32) -> Result<IMFAttributes> {
        if id != 0 {
            return Err(E_FAIL.into());
        }
        Ok(self.stream()?.to_interface())
    }

    /// Ignoring the D3D manager keeps samples in CPU memory (VCamSample's
    /// recommended setting for apps like Teams/Chrome that provide none).
    fn SetD3DManager(&self, _manager: windows_core::Ref<IUnknown>) -> Result<()> {
        Ok(())
    }
}

impl IMFGetService_Impl for MediaSource_Impl {
    fn GetService(
        &self,
        _service: *const GUID,
        _riid: *const GUID,
        _ppv: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

impl IMFSampleAllocatorControl_Impl for MediaSource_Impl {
    fn SetDefaultAllocator(
        &self,
        stream_id: u32,
        allocator: windows_core::Ref<IUnknown>,
    ) -> Result<()> {
        if stream_id != 0 {
            return Err(E_FAIL.into());
        }
        let a = allocator.as_ref().ok_or(E_POINTER)?;
        self.stream()?.set_allocator(a.cast()?);
        Ok(())
    }

    fn GetAllocatorUsage(
        &self,
        stream_id: u32,
        input_id: *mut u32,
        usage: *mut MFSampleAllocatorUsage,
    ) -> Result<()> {
        if input_id.is_null() || usage.is_null() {
            return Err(E_POINTER.into());
        }
        if stream_id != 0 {
            return Err(E_FAIL.into());
        }
        unsafe {
            *input_id = stream_id;
            *usage = MFSampleAllocatorUsage_UsesProvidedAllocator;
        }
        Ok(())
    }
}

impl IKsControl_Impl for MediaSource_Impl {
    fn KsProperty(
        &self,
        _p: *const KSIDENTIFIER,
        _pl: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> Result<()> {
        Err(KS_NOT_FOUND.into())
    }
    fn KsMethod(
        &self,
        _p: *const KSIDENTIFIER,
        _pl: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> Result<()> {
        Err(KS_NOT_FOUND.into())
    }
    fn KsEvent(
        &self,
        _p: *const KSIDENTIFIER,
        _pl: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> Result<()> {
        Err(KS_NOT_FOUND.into())
    }
}
