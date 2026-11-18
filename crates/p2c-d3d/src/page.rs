//! The page texture from Chromium's `on_accelerated_paint`: open the shared
//! NT handle on our device and copy it into a texture we own.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
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
    mismatch: Mutex<Option<u64>>,
}

struct PageState {
    tex: Texture,
    /// Opened shared textures by handle value (CEF reuses a small pool).
    cache: HashMap<isize, ID3D11Texture2D>,
}

fn page_texture(gpu: &Gpu, w: u32, h: u32) -> Result<Texture> {
    gpu.new_texture(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_SHADER_RESOURCE)
}

impl PageInput {
    /// `w` x `h` is the expected page size; a texture of another size
    /// replaces it (and empties the handle cache).
    pub fn new(gpu: &Gpu, w: u32, h: u32) -> Result<PageInput> {
        Ok(PageInput {
            inner: Arc::new(PageInner {
                gpu: gpu.clone(),
                state: Mutex::new(PageState {
                    tex: page_texture(gpu, w, h)?,
                    cache: HashMap::new(),
                }),
                seq: AtomicU64::new(0),
                mismatch: Mutex::new(None),
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
        let mut st = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        let shared = match st.cache.get(&handle) {
            Some(t) => t.clone(),
            None => {
                // SAFETY: CEF's handle is valid for the duration of the callback.
                let opened = unsafe {
                    gpu.device1()
                        .OpenSharedResource1::<ID3D11Texture2D>(HANDLE(handle as _))
                };
                let t = match opened {
                    Ok(t) => t,
                    Err(e) => {
                        let e = anyhow::Error::from(e);
                        if is_device_lost(&e) {
                            return Err(e.context("OpenSharedResource1: device lost"));
                        }
                        let mut mm = inner.mismatch.lock().unwrap_or_else(|p| p.into_inner());
                        if mm.is_none() {
                            *mm = open_on_matching_adapter(handle, &crate::enumerate())
                                .filter(|l| *l != gpu.luid());
                        }
                        return Err(match *mm {
                            Some(l) => e.context(format!(
                                "page texture is on adapter LUID {l:#x}, our device is on {:#x}",
                                gpu.luid()
                            )),
                            None => e.context("OpenSharedResource1 failed on every adapter"),
                        });
                    }
                };
                let d = tex_desc(&t);
                if d.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
                    bail!(
                        "page texture format {:?}, expected B8G8R8A8_UNORM",
                        d.Format
                    );
                }
                if (d.Width, d.Height) != (st.tex.width(), st.tex.height()) {
                    st.tex = page_texture(gpu, d.Width, d.Height)
                        .context("page texture for new size")?;
                    st.cache.clear();
                }
                st.cache.insert(handle, t.clone());
                t
            }
        };
        let ctx = gpu.context();
        // SAFETY: same size/format textures on our device; single context
        // calls are serialized by multithread protection.
        unsafe {
            ctx.CopyResource(st.tex.raw(), &shared);
            // Start the copy now: Chromium reuses the texture for later frames.
            ctx.Flush();
        }
        inner.seq.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Count of page frames copied so far.
    pub fn seq(&self) -> u64 {
        self.inner.seq.load(Ordering::Acquire)
    }

    /// The LUID of the adapter that does open Chromium's textures, when it is
    /// not ours (the device must be recreated there).
    pub fn adapter_mismatch(&self) -> Option<u64> {
        *self
            .inner
            .mismatch
            .lock()
            .unwrap_or_else(|p| p.into_inner())
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
