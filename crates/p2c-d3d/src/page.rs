//! The page texture from Chromium's `on_accelerated_paint`: open the shared
//! NT handle on our device and copy it into a texture we own.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use windows::core::Interface;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device1, ID3D11Texture2D, D3D11_BIND_SHADER_RESOURCE,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

use crate::adapters::AdapterInfo;
use crate::device::{create_device, is_device_lost, tex_desc, Gpu, Texture};

/// Try each hardware adapter's device on `handle`; the LUID of the first one
/// that opens it (`OpenSharedResource1` fails with `E_INVALIDARG` on the
/// others). Creates a throwaway device per adapter, so keep it off hot paths.
pub fn open_on_matching_adapter(handle: isize, adapters: &[AdapterInfo]) -> Option<u64> {
    adapters.iter().filter(|a| !a.software).find_map(|a| {
        let (device, _ctx) = create_device(a.luid).ok()?;
        let d1: ID3D11Device1 = device.cast().ok()?;
        // SAFETY: the handle is only read; the opened texture is dropped at once.
        unsafe { d1.OpenSharedResource1::<ID3D11Texture2D>(HANDLE(handle as _)) }
            .ok()
            .map(|_| a.luid)
    })
}

/// Receives Chromium's shared page textures (on the CEF UI thread) and keeps
/// the latest one in a texture of ours. Cloning shares it.
#[derive(Clone)]
pub struct PageInput {
    inner: Arc<PageInner>,
}

struct PageInner {
    gpu: Gpu,
    state: Mutex<PageState>,
    seq: AtomicU64,
    /// Result of probing the other adapters after a failed open: `None` =
    /// not probed since the last successful open.
    probe: Mutex<Option<Probe>>,
    probes: AtomicU32,
}

/// Which adapter opens Chromium's texture, when ours doesn't.
#[derive(Clone)]
struct Probe {
    /// Another adapter that opens it (`None`: no adapter does).
    mismatch: Option<u64>,
    /// Why our open failed, with adapter names.
    reason: String,
}

struct PageState {
    tex: Texture,
}

fn page_texture(gpu: &Gpu, w: u32, h: u32) -> Result<Texture> {
    gpu.new_texture(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_SHADER_RESOURCE)
}

impl PageInput {
    /// `w` x `h` is the expected page size; a texture of another size
    /// replaces it.
    pub fn new(gpu: &Gpu, w: u32, h: u32) -> Result<PageInput> {
        Ok(PageInput {
            inner: Arc::new(PageInner {
                gpu: gpu.clone(),
                state: Mutex::new(PageState {
                    tex: page_texture(gpu, w, h)?,
                }),
                seq: AtomicU64::new(0),
                probe: Mutex::new(None),
                probes: AtomicU32::new(0),
            }),
        })
    }

    /// Call from `on_accelerated_paint` with the shared NT handle (valid only
    /// during the callback). Queues a GPU copy into our texture and returns
    /// without waiting for the GPU; bumps [`PageInput::seq`] on success.
    /// A handle our device can't open is an error; if another adapter can
    /// open it, [`PageInput::adapter_mismatch`] reports that adapter.
    pub fn on_shared_texture(&self, handle: isize) -> Result<()> {
        let inner = &*self.inner;
        let gpu = &inner.gpu;
        // The state lock is never held across context calls (lock order:
        // a ticker may hold `gpu.lock()` and then call `texture()`).
        // Opened afresh on every callback: CEF closes the handle afterwards
        // and Windows reuses handle values, so a value seen before may name
        // a different texture (e.g. after Chromium rebuilds its pool on a
        // navigation). Caching by value showed stale pages.
        // SAFETY: CEF's handle is valid for the duration of the callback.
        let opened = unsafe {
            gpu.device1()
                .OpenSharedResource1::<ID3D11Texture2D>(HANDLE(handle as _))
        };
        let shared = match opened {
            Ok(t) => t,
            Err(e) => return Err(self.open_failed(handle, e.into())),
        };
        *inner.probe.lock().unwrap_or_else(|p| p.into_inner()) = None;
        let d = tex_desc(&shared);
        if d.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            bail!(
                "page texture format {:?}, expected B8G8R8A8_UNORM",
                d.Format
            );
        }
        let dst = {
            let mut st = inner.state.lock().unwrap_or_else(|p| p.into_inner());
            if (d.Width, d.Height) != (st.tex.width(), st.tex.height()) {
                st.tex =
                    page_texture(gpu, d.Width, d.Height).context("page texture for new size")?;
            }
            st.tex.clone()
        };
        let ctx = gpu.context();
        // SAFETY: same size/format textures on our device; single context
        // calls are serialized by multithread protection.
        unsafe {
            ctx.CopyResource(dst.raw(), &shared);
            // Start the copy now: Chromium reuses the texture for later frames.
            ctx.Flush();
        }
        // shared is released here: never kept past the callback.
        drop(shared);
        inner.seq.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// A page frame from the CPU (screen capture) instead of Chromium's
    /// texture: uploaded into our texture (resized to the frame); bumps
    /// [`PageInput::seq`].
    pub fn upload_frame(&self, f: &p2c_core::Frame) -> Result<()> {
        let inner = &*self.inner;
        let dst = {
            let mut st = inner.state.lock().unwrap_or_else(|p| p.into_inner());
            if (f.width, f.height) != (st.tex.width(), st.tex.height()) {
                st.tex = page_texture(&inner.gpu, f.width, f.height)
                    .context("page texture for new size")?;
            }
            st.tex.clone()
        };
        // The state lock is released before the context call (lock order).
        inner.gpu.update_bgra(&dst, f)?;
        inner.seq.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Our device couldn't open `handle`: find out (once, until the next
    /// successful open) which adapter can, and build the error.
    fn open_failed(&self, handle: isize, e: anyhow::Error) -> anyhow::Error {
        let inner = &*self.inner;
        let gpu = &inner.gpu;
        if is_device_lost(&e) {
            return e.context("OpenSharedResource1: device lost");
        }
        let mut probe = inner.probe.lock().unwrap_or_else(|p| p.into_inner());
        let reason = probe
            .get_or_insert_with(|| {
                inner.probes.fetch_add(1, Ordering::Relaxed);
                // A fresh list: LUIDs are never kept from an older one.
                let adapters = crate::enumerate();
                let name = |l: u64| {
                    adapters
                        .iter()
                        .find(|a| a.luid == l)
                        .map_or_else(|| format!("adapter LUID {l:#x}"), |a| a.name.clone())
                };
                let mismatch =
                    open_on_matching_adapter(handle, &adapters).filter(|l| *l != gpu.luid());
                let reason = match mismatch {
                    Some(l) => format!(
                        "Chromium renders on {}, our device is on {}",
                        name(l),
                        name(gpu.luid())
                    ),
                    None => format!(
                        "no GPU opens Chromium's page texture (our device: {})",
                        name(gpu.luid())
                    ),
                };
                Probe { mismatch, reason }
            })
            .reason
            .clone();
        e.context(reason)
    }

    /// How many times other adapters were probed for a handle (diagnostics/tests).
    #[doc(hidden)]
    pub fn probe_count(&self) -> u32 {
        self.inner.probes.load(Ordering::Relaxed)
    }

    /// Count of page frames copied so far.
    pub fn seq(&self) -> u64 {
        self.inner.seq.load(Ordering::Acquire)
    }

    /// The LUID of the adapter that does open Chromium's textures, when it is
    /// not ours (the device must be recreated there). Cleared by the next
    /// successful open.
    pub fn adapter_mismatch(&self) -> Option<u64> {
        self.inner
            .probe
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|p| p.mismatch)
    }

    /// Our copy of the latest page (BGRA, shader resource).
    pub fn texture(&self) -> Texture {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .tex
            .clone()
    }
}
