//! IMFMediaStream2: hands out one sample per RequestSample, filled from the
//! newest page2cam frame (or a "waiting for Page2Cam" pattern).
use std::sync::Mutex;

use windows::core::{implement, ComObject, IUnknown, Interface, Result, GUID, HRESULT};
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::KernelStreaming::{
    IKsControl, IKsControl_Impl, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;

use super::attrs::{forward_attributes, new_store};
use super::source::KS_NOT_FOUND;

const FPS: u32 = 30;
/// Frames older than this mean page2cam isn't running: show the waiting pattern.
const STALE_MS: u64 = 2000;
/// Offered formats; the first is the default.
const FORMATS: [(u32, u32, bool); 4] = [
    (1920, 1080, true),
    (1920, 1080, false),
    (1280, 720, true),
    (1280, 720, false),
];

#[implement(IMFMediaStream2, IKsControl, IMFAttributes)]
pub(crate) struct MediaStream {
    attrs: IMFAttributes,
    st: Mutex<State>,
}

struct State {
    queue: Option<IMFMediaEventQueue>,
    descriptor: Option<IMFStreamDescriptor>,
    source: Option<IMFMediaSource>,
    allocator: Option<IMFVideoSampleAllocatorEx>,
    state: MF_STREAM_STATE,
    nv12: bool,
    width: u32,
    height: u32,
    frame: Vec<u8>,
    frame_w: u32,
    frame_h: u32,
    frame_time_ms: u64,
    tick: u64,
    scratch: Vec<u8>,
}

forward_attributes!(MediaStream_Impl, attrs);

fn media_type(w: u32, h: u32, nv12: bool) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(
            &MF_MT_SUBTYPE,
            if nv12 {
                &MFVideoFormat_NV12
            } else {
                &MFVideoFormat_RGB32
            },
        )?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64)?;
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, if nv12 { w } else { w * 4 })?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        t.SetUINT64(&MF_MT_FRAME_RATE, ((FPS as u64) << 32) | 1)?;
        let bpp = if nv12 { 12 } else { 32 };
        t.SetUINT32(&MF_MT_AVG_BITRATE, w * h * bpp * FPS)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        Ok(t)
    }
}

pub(crate) fn create(index: u32) -> Result<ComObject<MediaStream>> {
    let attrs = new_store()?;
    unsafe {
        attrs.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
        attrs.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, index)?;
        attrs.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
        attrs.SetUINT32(
            &MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,
            MFFrameSourceTypes_Color.0 as u32,
        )?;
    }
    let types: Vec<Option<IMFMediaType>> = FORMATS
        .iter()
        .map(|&(w, h, nv)| media_type(w, h, nv).map(Some))
        .collect::<Result<_>>()?;
    let descriptor = unsafe { MFCreateStreamDescriptor(index, &types)? };
    unsafe {
        descriptor
            .GetMediaTypeHandler()?
            .SetCurrentMediaType(types[0].as_ref())?
    };
    let queue = unsafe { MFCreateEventQueue()? };
    let (w, h, nv12) = FORMATS[0];
    Ok(ComObject::new(MediaStream {
        attrs,
        st: Mutex::new(State {
            queue: Some(queue),
            descriptor: Some(descriptor),
            source: None,
            allocator: None,
            state: MF_STREAM_STATE_STOPPED,
            nv12,
            width: w,
            height: h,
            frame: Vec::new(),
            frame_w: 0,
            frame_h: 0,
            frame_time_ms: 0,
            tick: 0,
            scratch: Vec::new(),
        }),
    }))
}

impl MediaStream {
    pub(crate) fn descriptor(&self) -> Result<IMFStreamDescriptor> {
        self.st
            .lock()
            .unwrap()
            .descriptor
            .clone()
            .ok_or(MF_E_SHUTDOWN.into())
    }

    pub(crate) fn set_source(&self, s: IMFMediaSource) {
        self.st.lock().unwrap().source = Some(s);
    }

    pub(crate) fn set_allocator(&self, a: IMFVideoSampleAllocatorEx) {
        self.st.lock().unwrap().allocator = Some(a);
    }

    pub(crate) fn state(&self) -> MF_STREAM_STATE {
        self.st.lock().unwrap().state
    }

    pub(crate) fn start(&self, ty: Option<IMFMediaType>) -> Result<()> {
        let mut st = self.st.lock().unwrap();
        let queue = st.queue.clone().ok_or(MF_E_SHUTDOWN)?;
        let allocator = st.allocator.clone().ok_or(MF_E_SHUTDOWN)?;
        let ty = match ty {
            Some(t) => t,
            None => unsafe {
                st.descriptor
                    .as_ref()
                    .ok_or(MF_E_SHUTDOWN)?
                    .GetMediaTypeHandler()?
                    .GetCurrentMediaType()?
            },
        };
        unsafe {
            let sub = ty.GetGUID(&MF_MT_SUBTYPE)?;
            let size = ty.GetUINT64(&MF_MT_FRAME_SIZE)?;
            st.nv12 = sub == MFVideoFormat_NV12;
            st.width = (size >> 32) as u32;
            st.height = size as u32;
            allocator.InitializeSampleAllocator(10, &ty)?;
            queue.QueueEventParamVar(
                MEStreamStarted.0 as u32,
                &GUID::zeroed(),
                S_OK,
                std::ptr::null(),
            )?;
        }
        st.state = MF_STREAM_STATE_RUNNING;
        super::log(&format!(
            "stream started {}x{} {}",
            st.width,
            st.height,
            if st.nv12 { "NV12" } else { "RGB32" }
        ));
        Ok(())
    }

    pub(crate) fn stop(&self) -> Result<()> {
        let mut st = self.st.lock().unwrap();
        let queue = st.queue.clone().ok_or(MF_E_SHUTDOWN)?;
        if let Some(a) = &st.allocator {
            unsafe { a.UninitializeSampleAllocator()? };
        }
        unsafe {
            queue.QueueEventParamVar(
                MEStreamStopped.0 as u32,
                &GUID::zeroed(),
                S_OK,
                std::ptr::null(),
            )?
        };
        st.state = MF_STREAM_STATE_STOPPED;
        Ok(())
    }

    pub(crate) fn shutdown(&self) {
        let mut st = self.st.lock().unwrap();
        if let Some(q) = st.queue.take() {
            unsafe {
                let _ = q.Shutdown();
            }
        }
        st.descriptor = None;
        st.source = None;
        st.allocator = None;
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl State {
    /// Pick up the newest frame from page2cam, if any.
    fn refresh_frame(&mut self) {
        if let Some(base) = super::shared::base() {
            let mut buf = std::mem::take(&mut self.scratch);
            if let Some(info) = unsafe { p2c_shm::read_frame(base, &mut buf) } {
                if info.time_ms != self.frame_time_ms {
                    std::mem::swap(&mut self.frame, &mut buf);
                    self.frame_w = info.width;
                    self.frame_h = info.height;
                    self.frame_time_ms = info.time_ms;
                }
            }
            self.scratch = buf;
        }
    }

    /// BGRA source for this sample: the page2cam frame, or a waiting pattern.
    fn source_frame(&mut self) -> (Vec<u8>, u32, u32) {
        self.refresh_frame();
        self.tick += 1;
        if !self.frame.is_empty() && now_ms().saturating_sub(self.frame_time_ms) < STALE_MS {
            return (std::mem::take(&mut self.frame), self.frame_w, self.frame_h);
        }
        // Dark slate with a slow light band: "camera works, start Page2Cam".
        let (w, h) = (160u32, 90u32);
        let band = (self.tick / 2 % w as u64) as u32;
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for _y in 0..h {
            for x in 0..w {
                let v = if x.abs_diff(band) < 6 { 90 } else { 38 };
                px.extend_from_slice(&[v + 8, v + 4, v, 255]);
            }
        }
        (px, w, h)
    }

    fn fill(&mut self, sample: &IMFSample) -> Result<()> {
        let (src, sw, sh) = self.source_frame();
        let (w, h, nv12) = (self.width, self.height, self.nv12);
        // Build a tightly packed image, then copy rows using the buffer's pitch
        // (which may be negative for bottom-up RGB buffers).
        let (row_bytes, rows) = if nv12 {
            (w as usize, (h + h / 2) as usize)
        } else {
            (w as usize * 4, h as usize)
        };
        let mut packed = vec![0u8; row_bytes * rows];
        if nv12 {
            p2c_shm::fill_nv12(&src, sw, sh, &mut packed, row_bytes, w, h);
        } else {
            p2c_shm::fill_rgb32(&src, sw, sh, &mut packed, row_bytes, w, h);
        }
        if sw == self.frame_w && sh == self.frame_h && !src.is_empty() {
            self.frame = src; // give the page2cam frame back for reuse
        }
        unsafe {
            let buffer = sample.GetBufferByIndex(0)?;
            let b2: IMF2DBuffer2 = buffer.cast()?;
            let mut scan: *mut u8 = std::ptr::null_mut();
            let mut pitch: i32 = 0;
            let mut start: *mut u8 = std::ptr::null_mut();
            let mut len: u32 = 0;
            b2.Lock2DSize(
                MF2DBuffer_LockFlags_Write,
                &mut scan,
                &mut pitch,
                &mut start,
                &mut len,
            )?;
            for r in 0..rows {
                let dst = scan.offset(r as isize * pitch as isize);
                std::ptr::copy_nonoverlapping(
                    packed.as_ptr().add(r * row_bytes),
                    dst,
                    row_bytes.min(pitch.unsigned_abs() as usize),
                );
            }
            b2.Unlock2D()?;
        }
        Ok(())
    }
}

impl IMFMediaEventGenerator_Impl for MediaStream_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        let q = self.st.lock().unwrap().queue.clone().ok_or(MF_E_SHUTDOWN)?;
        unsafe { q.GetEvent(flags.0) }
    }
    fn BeginGetEvent(
        &self,
        cb: windows_core::Ref<IMFAsyncCallback>,
        state: windows_core::Ref<IUnknown>,
    ) -> Result<()> {
        let q = self.st.lock().unwrap().queue.clone().ok_or(MF_E_SHUTDOWN)?;
        unsafe { q.BeginGetEvent(cb.as_ref(), state.as_ref()) }
    }
    fn EndGetEvent(&self, result: windows_core::Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        let q = self.st.lock().unwrap().queue.clone().ok_or(MF_E_SHUTDOWN)?;
        unsafe { q.EndGetEvent(result.as_ref()) }
    }
    fn QueueEvent(
        &self,
        met: u32,
        ext: *const GUID,
        hr: HRESULT,
        v: *const PROPVARIANT,
    ) -> Result<()> {
        let q = self.st.lock().unwrap().queue.clone().ok_or(MF_E_SHUTDOWN)?;
        unsafe { q.QueueEventParamVar(met, ext, hr, v) }
    }
}

impl IMFMediaStream_Impl for MediaStream_Impl {
    fn GetMediaSource(&self) -> Result<IMFMediaSource> {
        self.st
            .lock()
            .unwrap()
            .source
            .clone()
            .ok_or(MF_E_SHUTDOWN.into())
    }

    fn GetStreamDescriptor(&self) -> Result<IMFStreamDescriptor> {
        self.descriptor()
    }

    fn RequestSample(&self, token: windows_core::Ref<IUnknown>) -> Result<()> {
        let mut st = self.st.lock().unwrap();
        let queue = st.queue.clone().ok_or(MF_E_SHUTDOWN)?;
        let allocator = st.allocator.clone().ok_or(MF_E_SHUTDOWN)?;
        let r = (|| unsafe {
            let sample = allocator.AllocateSample()?;
            sample.SetSampleTime(MFGetSystemTime())?;
            sample.SetSampleDuration(10_000_000 / FPS as i64)?;
            st.fill(&sample)?;
            if let Some(t) = token.as_ref() {
                sample.SetUnknown(&MFSampleExtension_Token, t)?;
            }
            queue.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &sample)
        })();
        if let Err(e) = &r {
            if st.tick.is_multiple_of(300) {
                super::log(&format!("RequestSample failed: {e}"));
            }
        }
        r
    }
}

impl IMFMediaStream2_Impl for MediaStream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> Result<()> {
        let current = self.state();
        if current == value {
            return Ok(());
        }
        match value {
            MF_STREAM_STATE_PAUSED if current == MF_STREAM_STATE_RUNNING => {
                self.st.lock().unwrap().state = value;
                Ok(())
            }
            MF_STREAM_STATE_RUNNING => self.start(None),
            MF_STREAM_STATE_STOPPED => self.stop(),
            _ => Err(MF_E_INVALID_STATE_TRANSITION.into()),
        }
    }

    fn GetStreamState(&self) -> Result<MF_STREAM_STATE> {
        Ok(self.state())
    }
}

impl IKsControl_Impl for MediaStream_Impl {
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
