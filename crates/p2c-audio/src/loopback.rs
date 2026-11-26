//! One process tree's sound (the window background's app) → 48 kHz stereo ring.
//!
//! Windows: WASAPI process loopback (`VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK`,
//! `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`), so a browser's sound,
//! which comes from a child process, is included. Elsewhere `start` fails.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use p2c_core::AudioRing;
use parking_lot::Mutex;

/// Captures the sound of one process and its children while it lives.
/// Dropping it stops the capture.
pub struct ProcessLoopback {
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl ProcessLoopback {
    /// Start capturing `pid`'s process tree into `ring`. Fails if the audio
    /// client can't be activated or started (the reason is in the error).
    pub fn start(pid: u32, ring: AudioRing) -> anyhow::Result<ProcessLoopback> {
        imp::start(pid, ring)
    }

    /// Why capture stopped after a successful start, if it did.
    pub fn error(&self) -> Option<String> {
        self.error.lock().clone()
    }
}

impl Drop for ProcessLoopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    /// Same text as the window background's error elsewhere.
    const WINDOW_UNSUPPORTED: &str = "window capture is Windows-only for now";

    pub(super) fn start(_pid: u32, _ring: AudioRing) -> anyhow::Result<ProcessLoopback> {
        anyhow::bail!(WINDOW_UNSUPPORTED)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    use anyhow::Context;
    use p2c_core::SAMPLE_RATE;
    use windows::core::{implement, Interface, Ref, HRESULT};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Media::Audio::{
        ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
        IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
        IAudioCaptureClient, IAudioClient, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
        AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
        PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        WAVEFORMATEX,
    };
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, BLOB, COINIT_MULTITHREADED};
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows::Win32::System::Variant::VT_BLOB;

    use crate::mic::to_stereo;
    use crate::Resampler;

    const WAVE_FORMAT_PCM: u16 = 1;
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    /// 200 ms shared-mode buffer (100 ns units).
    const BUFFER_HNS: i64 = 2_000_000;

    #[derive(Clone, Copy)]
    enum Sample {
        F32,
        I16,
    }

    /// Formats asked for, in order. Process-loopback clients have no mix
    /// format; with AUTOCONVERTPCM the engine converts to what we ask.
    const FORMATS: [(Sample, u32); 3] = [
        (Sample::F32, SAMPLE_RATE),
        (Sample::I16, SAMPLE_RATE),
        (Sample::I16, 44_100),
    ];

    fn wave_format(sample: Sample, rate: u32) -> WAVEFORMATEX {
        let (tag, bits) = match sample {
            Sample::F32 => (WAVE_FORMAT_IEEE_FLOAT, 32u16),
            Sample::I16 => (WAVE_FORMAT_PCM, 16u16),
        };
        let block = 2 * bits / 8;
        WAVEFORMATEX {
            wFormatTag: tag,
            nChannels: 2,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * block as u32,
            nBlockAlign: block,
            wBitsPerSample: bits,
            cbSize: 0,
        }
    }

    #[implement(IActivateAudioInterfaceCompletionHandler)]
    struct Done(Mutex<Option<mpsc::Sender<()>>>);

    impl IActivateAudioInterfaceCompletionHandler_Impl for Done_Impl {
        fn ActivateCompleted(
            &self,
            _op: Ref<IActivateAudioInterfaceAsyncOperation>,
        ) -> windows::core::Result<()> {
            if let Some(tx) = self.0.lock().take() {
                let _ = tx.send(());
            }
            Ok(())
        }
    }

    /// Owns the COM apartment and the event for the capture thread.
    struct Session {
        event: HANDLE,
    }

    impl Drop for Session {
        fn drop(&mut self) {
            unsafe {
                if !self.event.is_invalid() {
                    let _ = CloseHandle(self.event);
                }
                CoUninitialize();
            }
        }
    }

    struct Opened {
        client: IAudioClient,
        capture: IAudioCaptureClient,
        sample: Sample,
        rate: u32,
    }

    fn activate(pid: u32) -> anyhow::Result<IAudioClient> {
        let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };
        // ManuallyDrop: PROPVARIANT's Drop runs PropVariantClear, which would
        // CoTaskMemFree the blob, but the blob is `params` on our stack.
        let mut pv = std::mem::ManuallyDrop::new(PROPVARIANT::default());
        // SAFETY: plain union writes; the blob points at `params`, which
        // outlives the activation (we wait for it to complete below).
        unsafe {
            let inner = &mut *pv.Anonymous.Anonymous;
            inner.vt = VT_BLOB;
            inner.Anonymous.blob = BLOB {
                cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                pBlobData: &mut params as *mut _ as *mut u8,
            };
        }
        let (tx, rx) = mpsc::channel();
        let handler: IActivateAudioInterfaceCompletionHandler = Done(Mutex::new(Some(tx))).into();
        let op = unsafe {
            ActivateAudioInterfaceAsync(
                VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                &IAudioClient::IID,
                Some(&*pv),
                &handler,
            )
        }
        .context("ActivateAudioInterfaceAsync")?;
        rx.recv_timeout(Duration::from_secs(5))
            .context("audio activation did not complete")?;
        let mut hr = HRESULT(0);
        let mut unknown = None;
        unsafe { op.GetActivateResult(&mut hr, &mut unknown) }.context("GetActivateResult")?;
        hr.ok().context("activating process loopback")?;
        unknown
            .context("activation returned no interface")?
            .cast::<IAudioClient>()
            .context("not an IAudioClient")
    }

    fn open(pid: u32, event: HANDLE) -> anyhow::Result<Opened> {
        let mut last = None;
        for (sample, rate) in FORMATS {
            // A failed Initialize leaves the client unusable: activate afresh.
            let client = activate(pid)?;
            let fmt = wave_format(sample, rate);
            let init = unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK
                        | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                    BUFFER_HNS,
                    0,
                    &fmt,
                    None,
                )
            };
            match init {
                Ok(()) => {
                    unsafe { client.SetEventHandle(event) }.context("SetEventHandle")?;
                    let capture: IAudioCaptureClient =
                        unsafe { client.GetService() }.context("IAudioCaptureClient")?;
                    unsafe { client.Start() }.context("starting capture")?;
                    return Ok(Opened {
                        client,
                        capture,
                        sample,
                        rate,
                    });
                }
                Err(e) => last = Some(e),
            }
        }
        Err(anyhow::Error::from(last.expect("at least one format")))
            .context("initializing process loopback")
    }

    /// Read every queued packet, convert to 48 kHz stereo f32, push.
    fn drain(o: &Opened, rs: &mut Resampler, ring: &AudioRing) -> windows::core::Result<()> {
        loop {
            let frames = unsafe { o.capture.GetNextPacketSize()? };
            if frames == 0 {
                return Ok(());
            }
            let mut data = std::ptr::null_mut();
            let mut n = 0u32;
            let mut flags = 0u32;
            unsafe {
                o.capture
                    .GetBuffer(&mut data, &mut n, &mut flags, None, None)?
            };
            let count = n as usize * 2;
            let samples: Vec<f32> =
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    vec![0.0; count]
                } else {
                    // SAFETY: the engine hands `n` frames of the format we
                    // initialised with (2 channels), valid until ReleaseBuffer.
                    unsafe {
                        match o.sample {
                            Sample::F32 => {
                                std::slice::from_raw_parts(data as *const f32, count).to_vec()
                            }
                            Sample::I16 => std::slice::from_raw_parts(data as *const i16, count)
                                .iter()
                                .map(|s| *s as f32 / 32768.0)
                                .collect(),
                        }
                    }
                };
            unsafe { o.capture.ReleaseBuffer(n)? };
            ring.push(&rs.process(&to_stereo(&samples, 2)));
        }
    }

    pub(super) fn start(pid: u32, ring: AudioRing) -> anyhow::Result<ProcessLoopback> {
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None::<String>));
        let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<()>>();
        let (stop2, error2) = (stop.clone(), error.clone());
        let thread = std::thread::Builder::new()
            .name("p2c-loopback".into())
            .spawn(move || {
                // Activation must happen on an MTA thread.
                let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                if let Err(e) = hr.ok() {
                    let _ = ready_tx.send(Err(anyhow::Error::from(e).context("CoInitializeEx")));
                    return;
                }
                let mut session = Session {
                    event: HANDLE::default(),
                };
                session.event = match unsafe { CreateEventW(None, false, false, None) } {
                    Ok(h) => h,
                    Err(e) => {
                        let _ = ready_tx.send(Err(anyhow::Error::from(e).context("CreateEventW")));
                        return;
                    }
                };
                let opened = match open(pid, session.event) {
                    Ok(o) => o,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                tracing::info!(
                    pid,
                    rate = opened.rate,
                    float = matches!(opened.sample, Sample::F32),
                    "window sound capture started"
                );
                let _ = ready_tx.send(Ok(()));
                let mut rs = Resampler::new(opened.rate, SAMPLE_RATE);
                while !stop2.load(Ordering::Relaxed) {
                    let w = unsafe { WaitForSingleObject(session.event, 100) };
                    if w != WAIT_OBJECT_0 {
                        continue;
                    }
                    if let Err(e) = drain(&opened, &mut rs, &ring) {
                        tracing::warn!(pid, error = %e, "window sound capture stopped");
                        *error2.lock() = Some(e.to_string());
                        break;
                    }
                }
                let _ = unsafe { opened.client.Stop() };
                drop(opened);
                drop(session);
            })?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(ProcessLoopback {
                stop,
                error,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                anyhow::bail!("window sound thread exited")
            }
        }
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_elsewhere() {
        let e = ProcessLoopback::start(1, AudioRing::new(100))
            .err()
            .expect("must fail");
        assert_eq!(e.to_string(), "window capture is Windows-only for now");
    }
}
