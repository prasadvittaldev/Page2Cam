//! The D3D11 device ([`Gpu`]), the [`Texture`] wrapper and small synchronous
//! helpers (upload, read back) used by tests and the compositor.
//!
//! One immediate context is shared between threads (the CEF UI thread copies
//! page textures, the ticker composites), so the device is created with
//! `ID3D11Multithread::SetMultithreadProtected(TRUE)`: every context call is
//! serialized. Sequences of calls that set pipeline state must hold
//! [`Gpu::lock`] so another thread can't interleave its own state.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use p2c_core::{Frame, Nv12Frame};
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11DeviceContext, ID3D11Multithread,
    ID3D11RenderTargetView, ID3D11ShaderResourceView, ID3D11Texture2D, D3D11_BIND_FLAG,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory4, IDXGIResource1, DXGI_ERROR_DEVICE_HUNG,
    DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_DRIVER_INTERNAL_ERROR,
    DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE,
};

use crate::shaders::Shaders;

/// A 2D texture on a [`Gpu`] plus the views its bind flags allow.
/// Cloning is cheap (COM reference counts).
#[derive(Clone)]
pub struct Texture {
    tex: ID3D11Texture2D,
    w: u32,
    h: u32,
    format: DXGI_FORMAT,
    srv: Option<ID3D11ShaderResourceView>,
    rtv: Option<ID3D11RenderTargetView>,
}

// SAFETY: D3D11 resources and views are free-threaded objects; only the
// immediate context is not, and that is guarded by multithread protection.
unsafe impl Send for Texture {}
unsafe impl Sync for Texture {}

impl Texture {
    pub fn width(&self) -> u32 {
        self.w
    }
    pub fn height(&self) -> u32 {
        self.h
    }
    pub fn format(&self) -> DXGI_FORMAT {
        self.format
    }
    pub fn raw(&self) -> &ID3D11Texture2D {
        &self.tex
    }
    /// Shader resource view (textures created with `BIND_SHADER_RESOURCE`).
    pub fn srv(&self) -> Option<&ID3D11ShaderResourceView> {
        self.srv.as_ref()
    }
    /// Render target view (textures created with `BIND_RENDER_TARGET`).
    pub fn rtv(&self) -> Option<&ID3D11RenderTargetView> {
        self.rtv.as_ref()
    }
}

/// True if `e` (or anything in its cause chain) says the GPU device is gone
/// (`DXGI_ERROR_DEVICE_REMOVED` and friends): the device must be recreated.
pub fn is_device_lost(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<windows::core::Error>().is_some_and(|w| {
            [
                DXGI_ERROR_DEVICE_REMOVED,
                DXGI_ERROR_DEVICE_RESET,
                DXGI_ERROR_DEVICE_HUNG,
                DXGI_ERROR_DRIVER_INTERNAL_ERROR,
            ]
            .contains(&w.code())
        })
    })
}

/// A D3D11 device and its immediate context on one adapter. Cloning shares it.
#[derive(Clone)]
pub struct Gpu {
    inner: Arc<GpuInner>,
}

struct GpuInner {
    luid: u64,
    device: ID3D11Device,
    device1: ID3D11Device1,
    ctx: ID3D11DeviceContext,
    mt: ID3D11Multithread,
    pub(crate) shaders: Shaders,
    pub(crate) nv12_targets: std::sync::Mutex<Option<(Texture, Texture)>>,
}

// SAFETY: the device is free-threaded; the immediate context is made
// thread-safe with SetMultithreadProtected(TRUE), and multi-call sequences
// take `Gpu::lock` (ID3D11Multithread::Enter/Leave).
unsafe impl Send for GpuInner {}
unsafe impl Sync for GpuInner {}

/// Holds the context's multithread lock (re-entrant); released on drop.
pub struct ContextLock<'a>(&'a ID3D11Multithread);

impl Drop for ContextLock<'_> {
    fn drop(&mut self) {
        // SAFETY: paired with the Enter in `Gpu::lock`.
        unsafe { self.0.Leave() }
    }
}

/// A CEF-like shared texture (see [`Gpu::create_shared_bgra`]). Closes its NT
/// handle on drop.
pub struct SharedTexture {
    handle: HANDLE,
    _tex: ID3D11Texture2D,
}

impl SharedTexture {
    pub fn handle(&self) -> isize {
        self.handle.0 as isize
    }
}

impl Drop for SharedTexture {
    fn drop(&mut self) {
        // SAFETY: we own the handle from CreateSharedHandle.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

pub(crate) fn win_luid(luid: u64) -> LUID {
    LUID {
        LowPart: luid as u32,
        HighPart: (luid >> 32) as u32 as i32,
    }
}

/// A plain D3D11 device (BGRA support) on the adapter with this LUID.
pub(crate) fn create_device(luid: u64) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    // SAFETY: COM calls with valid out-pointers.
    unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory1().context("CreateDXGIFactory1")?;
        let adapter: IDXGIAdapter1 = factory
            .EnumAdapterByLuid(win_luid(luid))
            .with_context(|| format!("no adapter with LUID {luid:#x}"))?;
        let mut device = None;
        let mut ctx = None;
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .with_context(|| format!("D3D11CreateDevice on adapter {luid:#x}"))?;
        Ok((
            device.context("D3D11CreateDevice returned no device")?,
            ctx.context("D3D11CreateDevice returned no context")?,
        ))
    }
}

fn bytes_per_pixel(f: DXGI_FORMAT) -> Result<usize> {
    Ok(match f {
        DXGI_FORMAT_R8_UNORM => 1,
        DXGI_FORMAT_R8G8_UNORM => 2,
        DXGI_FORMAT_B8G8R8A8_UNORM => 4,
        other => bail!("unsupported texture format {other:?}"),
    })
}

pub(crate) fn tex_desc(t: &ID3D11Texture2D) -> D3D11_TEXTURE2D_DESC {
    let mut d = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: valid out-pointer.
    unsafe { t.GetDesc(&mut d) };
    d
}

impl Gpu {
    /// Create the device on the adapter with this LUID (from a fresh
    /// [`crate::enumerate`]) and compile the shaders.
    pub fn new(luid: u64) -> Result<Gpu> {
        let (device, ctx) = create_device(luid)?;
        let device1: ID3D11Device1 = device.cast().context("ID3D11Device1")?;
        let mt: ID3D11Multithread = ctx.cast().context("ID3D11Multithread")?;
        // SAFETY: plain COM call.
        unsafe {
            let _ = mt.SetMultithreadProtected(true);
        }
        let shaders = Shaders::new(&device)?;
        Ok(Gpu {
            inner: Arc::new(GpuInner {
                luid,
                device,
                device1,
                ctx,
                mt,
                shaders,
                nv12_targets: std::sync::Mutex::new(None),
            }),
        })
    }

    pub fn luid(&self) -> u64 {
        self.inner.luid
    }

    /// True if both handles share the same device (not merely the same
    /// adapter: a device recreated after a loss is a different one).
    pub fn same(&self, other: &Gpu) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.inner.device
    }

    pub(crate) fn device1(&self) -> &ID3D11Device1 {
        &self.inner.device1
    }

    /// The shared immediate context. Single calls are thread-safe; hold
    /// [`Gpu::lock`] around sequences that set pipeline state.
    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.inner.ctx
    }

    pub(crate) fn shaders(&self) -> &Shaders {
        &self.inner.shaders
    }

    pub(crate) fn nv12_targets(&self) -> &std::sync::Mutex<Option<(Texture, Texture)>> {
        &self.inner.nv12_targets
    }

    /// Exclusive use of the immediate context until the guard drops.
    pub fn lock(&self) -> ContextLock<'_> {
        // SAFETY: paired with Leave in ContextLock::drop.
        unsafe { self.inner.mt.Enter() };
        ContextLock(&self.inner.mt)
    }

    /// True once the device has been removed/reset (driver update, TDR, ...).
    pub fn is_lost(&self) -> bool {
        // SAFETY: plain COM call.
        unsafe { self.inner.device.GetDeviceRemovedReason().is_err() }
    }

    /// New `DEFAULT`-usage texture with the views matching `bind`.
    pub fn new_texture(
        &self,
        w: u32,
        h: u32,
        format: DXGI_FORMAT,
        bind: D3D11_BIND_FLAG,
    ) -> Result<Texture> {
        self.texture_with(w, h, format, bind.0 as u32, 0, None)
    }

    fn texture_with(
        &self,
        w: u32,
        h: u32,
        format: DXGI_FORMAT,
        bind: u32,
        misc: u32,
        init: Option<&[u8]>,
    ) -> Result<Texture> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind,
            CPUAccessFlags: 0,
            MiscFlags: misc,
        };
        let data = init.map(|d| D3D11_SUBRESOURCE_DATA {
            pSysMem: d.as_ptr() as _,
            SysMemPitch: w * bytes_per_pixel(format).unwrap_or(4) as u32,
            SysMemSlicePitch: 0,
        });
        let mut tex = None;
        // SAFETY: desc/data outlive the call; `init` holds w*h*bpp bytes (callers check).
        unsafe {
            self.inner
                .device
                .CreateTexture2D(&desc, data.as_ref().map(|d| d as *const _), Some(&mut tex))
                .with_context(|| format!("CreateTexture2D {w}x{h} {format:?}"))?;
        }
        self.wrap(tex.context("CreateTexture2D returned nothing")?)
    }

    /// Wrap an existing texture of this device, creating the views its bind
    /// flags allow.
    pub fn wrap(&self, tex: ID3D11Texture2D) -> Result<Texture> {
        let d = tex_desc(&tex);
        let mut srv = None;
        let mut rtv = None;
        // SAFETY: COM calls with valid out-pointers.
        unsafe {
            if d.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 != 0 {
                self.inner
                    .device
                    .CreateShaderResourceView(&tex, None, Some(&mut srv))
                    .context("CreateShaderResourceView")?;
            }
            if d.BindFlags & D3D11_BIND_RENDER_TARGET.0 as u32 != 0 {
                self.inner
                    .device
                    .CreateRenderTargetView(&tex, None, Some(&mut rtv))
                    .context("CreateRenderTargetView")?;
            }
        }
        Ok(Texture {
            tex,
            w: d.Width,
            h: d.Height,
            format: d.Format,
            srv,
            rtv,
        })
    }

    /// Upload a BGRA frame to a new shader-readable texture.
    pub fn upload_bgra(&self, f: &Frame) -> Result<Texture> {
        if f.data.len() != f.width as usize * f.height as usize * 4 {
            bail!("frame buffer does not match {}x{}", f.width, f.height);
        }
        self.texture_with(
            f.width,
            f.height,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            D3D11_BIND_SHADER_RESOURCE.0 as u32,
            0,
            Some(&f.data),
        )
    }

    /// Overwrite a BGRA texture (same size as `f`) with a frame.
    pub fn update_bgra(&self, t: &Texture, f: &Frame) -> Result<()> {
        if (t.w, t.h) != (f.width, f.height) || t.format != DXGI_FORMAT_B8G8R8A8_UNORM {
            bail!("update_bgra: texture/frame mismatch");
        }
        if f.data.len() != f.width as usize * f.height as usize * 4 {
            bail!("frame buffer does not match {}x{}", f.width, f.height);
        }
        // SAFETY: the buffer holds height rows of width*4 bytes.
        unsafe {
            self.inner
                .ctx
                .UpdateSubresource(&t.tex, 0, None, f.data.as_ptr() as _, f.width * 4, 0);
        }
        Ok(())
    }

    /// A texture shaped like the one CEF's `on_accelerated_paint` hands out
    /// (BGRA, render target + shader resource, `SHARED | SHARED_NTHANDLE`),
    /// filled with `f`, plus an NT handle to it. For tests and diagnostics.
    pub fn create_shared_bgra(&self, f: &Frame) -> Result<SharedTexture> {
        if f.data.len() != f.width as usize * f.height as usize * 4 {
            bail!("frame buffer does not match {}x{}", f.width, f.height);
        }
        let t = self.texture_with(
            f.width,
            f.height,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
            Some(&f.data),
        )?;
        let res: IDXGIResource1 = t.tex.cast().context("IDXGIResource1")?;
        // SAFETY: plain COM calls.
        unsafe {
            let handle = res
                .CreateSharedHandle(
                    None,
                    DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                    PCWSTR::null(),
                )
                .context("CreateSharedHandle")?;
            self.inner.ctx.Flush();
            Ok(SharedTexture {
                handle,
                _tex: t.tex,
            })
        }
    }

    /// A CPU-readable (staging) texture of this size and format.
    pub(crate) fn staging_like(
        &self,
        w: u32,
        h: u32,
        format: DXGI_FORMAT,
    ) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut tex = None;
        // SAFETY: valid desc and out-pointer.
        unsafe {
            self.inner
                .device
                .CreateTexture2D(&desc, None, Some(&mut tex))
                .with_context(|| format!("staging texture {w}x{h}"))?;
        }
        tex.context("CreateTexture2D returned nothing")
    }

    /// Map a staging texture and append its rows (tightly packed) to `out`.
    pub(crate) fn read_staging(&self, st: &ID3D11Texture2D, out: &mut Vec<u8>) -> Result<()> {
        let d = tex_desc(st);
        let row = d.Width as usize * bytes_per_pixel(d.Format)?;
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the mapping holds Height rows of RowPitch bytes until Unmap.
        unsafe {
            self.inner
                .ctx
                .Map(st, 0, D3D11_MAP_READ, 0, Some(&mut m))
                .context("Map staging texture")?;
            let base = m.pData as *const u8;
            for y in 0..d.Height as usize {
                let p = std::slice::from_raw_parts(base.add(y * m.RowPitch as usize), row);
                out.extend_from_slice(p);
            }
            self.inner.ctx.Unmap(st, 0);
        }
        Ok(())
    }

    /// Copy `t` to the CPU, waiting for the GPU (tests, diagnostics).
    fn read_tex(&self, t: &Texture, out: &mut Vec<u8>) -> Result<()> {
        let st = self.staging_like(t.w, t.h, t.format)?;
        // SAFETY: same size/format resources of this device.
        unsafe { self.inner.ctx.CopyResource(&st, &t.tex) };
        self.read_staging(&st, out)
    }

    /// Read a BGRA texture back synchronously (tests, diagnostics).
    pub fn read_bgra(&self, t: &Texture) -> Result<Frame> {
        if t.format != DXGI_FORMAT_B8G8R8A8_UNORM {
            bail!("read_bgra: texture is {:?}", t.format);
        }
        let mut data = Vec::with_capacity(t.w as usize * t.h as usize * 4);
        self.read_tex(t, &mut data)?;
        Ok(Frame::from_bgra(t.w, t.h, data)?)
    }

    /// Read a Y (R8) + UV (R8G8, half size) pair back synchronously.
    pub fn read_nv12(&self, y: &Texture, uv: &Texture) -> Result<Nv12Frame> {
        check_nv12_pair(y, uv)?;
        let mut data = Vec::with_capacity(y.w as usize * y.h as usize * 3 / 2);
        self.read_tex(y, &mut data)?;
        self.read_tex(uv, &mut data)?;
        Ok(Nv12Frame::new(y.w, y.h, data)?)
    }
}

pub(crate) fn check_nv12_pair(y: &Texture, uv: &Texture) -> Result<()> {
    if y.format != DXGI_FORMAT_R8_UNORM
        || uv.format != DXGI_FORMAT_R8G8_UNORM
        || (uv.w * 2, uv.h * 2) != (y.w, y.h)
    {
        bail!(
            "not an NV12 plane pair: Y {}x{} {:?}, UV {}x{} {:?}",
            y.w,
            y.h,
            y.format,
            uv.w,
            uv.h,
            uv.format
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::E_INVALIDARG;

    #[test]
    fn device_lost_detected_through_context() {
        let e: Result<()> = Err(windows::core::Error::from(DXGI_ERROR_DEVICE_REMOVED).into());
        let e = e.context("Map staging texture").unwrap_err();
        assert!(is_device_lost(&e));
        let e: Result<()> = Err(windows::core::Error::from(E_INVALIDARG).into());
        assert!(!is_device_lost(&e.context("open").unwrap_err()));
        assert!(!is_device_lost(&anyhow::anyhow!("plain error")));
    }
}
