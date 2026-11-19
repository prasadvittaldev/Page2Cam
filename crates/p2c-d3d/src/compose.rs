//! The GPU compositor: page (from [`PageInput`]) letterboxed into a BGRA
//! render target, the webcam PIP on top (Box or cut-out with the person mask),
//! then NV12 for the stream and the camera, read back through
//! [`Readback`] rings, and a small BGRA preview when one is due.
//!
//! The PIP (webcam x mask, premultiplied) is rendered into a layer texture
//! at the PIP rectangle's size only when the webcam frame, mask, style or
//! size changes; every composite copies the page into place and draws the
//! cached layer 1:1 (R41).
//!
//! The BGRA intermediate matches `p2c_compositor::Compositor` (same
//! `fit_rect`/`pip_rect` geometry and rounding): within ±2 per channel where
//! the exact filter is used (unscaled, downscaled, letterbox), within 8 (mean
//! ≤ 1) where an upscaled webcam/mask uses the hardware bilinear sampler.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use p2c_compositor::{
    fit_rect, pip_rect, Output, OutputFormat, OutputProducer, OutputSpec, OverlayStyle, PipLayout,
    TickInputs,
};
use p2c_core::{camera_size, EdgeRange, Frame, Mask, Nv12Frame};
use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11BlendState, ID3D11Buffer, ID3D11PixelShader, ID3D11ShaderResourceView, ID3D11Texture2D,
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BLEND_DESC, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD,
    D3D11_BLEND_ZERO, D3D11_BUFFER_DESC, D3D11_COLOR_WRITE_ENABLE_ALL,
    D3D11_RENDER_TARGET_BLEND_DESC, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM,
};

use crate::device::{Gpu, Texture};
use crate::page::PageInput;
use crate::profile::Profiler;
use crate::readback::Readback;
use crate::shaders::{bgra_to_nv12_into, ps};

const COMPOSE_HLSL: &str = include_str!("hlsl/compose.hlsl");

/// Largest admin preview (fitted, aspect kept; never larger than the output).
const PREVIEW_MAX: (u32, u32) = (960, 540);

/// Constants of one draw (`cbuffer PassConsts` in compose.hlsl), 64 bytes.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PassConsts {
    rect: [f32; 4],
    src_size: [f32; 2],
    mask_size: [f32; 2],
    mode: u32,
    edge_lo: f32,
    edge_hi: f32,
    edge_full: u32,
    /// Scale the webcam / mask with the hardware bilinear sampler.
    cam_linear: u32,
    mask_linear: u32,
    pad: [f32; 2],
}

const MODE_OPAQUE: u32 = 0;
const MODE_CUTOUT: u32 = 1;

/// Shaders and fixed state of the compose passes.
struct Pipeline {
    /// Exact-filter page/PIP scaling (also builds the PIP layer).
    ps: ID3D11PixelShader,
    /// The cached PIP layer, 1:1.
    ps_layer: ID3D11PixelShader,
    /// Plain bilinear scaling (preview).
    ps_sample: ID3D11PixelShader,
    /// Premultiplied "over" for colour; destination alpha kept (opaque).
    blend: ID3D11BlendState,
    /// One constant buffer per draw of a frame (page, layer build, layer
    /// draw, preview).
    cbs: [ID3D11Buffer; 4],
}

impl Pipeline {
    fn new(gpu: &Gpu) -> Result<Pipeline> {
        let device = gpu.device();
        let ps = ps(device, COMPOSE_HLSL, "compose.hlsl", "ps_compose")?;
        let mut bd = D3D11_BLEND_DESC::default();
        bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            SrcBlend: D3D11_BLEND_ONE,
            DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: D3D11_BLEND_ZERO,
            DestBlendAlpha: D3D11_BLEND_ONE,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
        };
        let mut blend = None;
        // SAFETY: valid desc and out-pointer.
        unsafe { device.CreateBlendState(&bd, Some(&mut blend)) }.context("CreateBlendState")?;
        let cb = || -> Result<ID3D11Buffer> {
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<PassConsts>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                StructureByteStride: 0,
            };
            let mut b = None;
            // SAFETY: valid desc and out-pointer.
            unsafe { device.CreateBuffer(&desc, None, Some(&mut b)) }.context("CreateBuffer")?;
            b.context("CreateBuffer returned nothing")
        };
        Ok(Pipeline {
            ps,
            ps_layer: crate::shaders::ps(device, COMPOSE_HLSL, "compose.hlsl", "ps_layer")?,
            ps_sample: crate::shaders::ps(device, COMPOSE_HLSL, "compose.hlsl", "ps_sample")?,
            blend: blend.context("CreateBlendState returned nothing")?,
            cbs: [cb()?, cb()?, cb()?, cb()?],
        })
    }
}

fn viewport(x: f32, y: f32, w: f32, h: f32) -> D3D11_VIEWPORT {
    D3D11_VIEWPORT {
        TopLeftX: x,
        TopLeftY: y,
        Width: w,
        Height: h,
        MinDepth: 0.0,
        MaxDepth: 1.0,
    }
}

/// An uploaded input and the Arc it came from (re-uploaded when that changes).
struct Uploaded<T> {
    src: Arc<T>,
    tex: Texture,
}

/// The inputs a composite was made from; identity comparison like the CPU
/// compositor (Arc pointers, page sequence number, layout).
struct Key {
    page_seq: u64,
    cam: Option<Arc<Frame>>,
    mask: Option<Arc<Mask>>,
    layout: Option<PipLayout>,
}

fn same<T>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

impl Key {
    /// The effective inputs: the mask only for cut-out styles, and a cut-out
    /// without a mask becomes a Box (as in the CPU compositor).
    fn of(page_seq: u64, inputs: &TickInputs) -> Key {
        let cam = inputs.cam.clone().filter(|c| c.width > 0 && c.height > 0);
        let mut layout = inputs.layout;
        let mask = if cam.is_some() && layout.style.is_cutout() {
            inputs.mask.clone()
        } else {
            None
        };
        if layout.style.is_cutout() && mask.is_none() {
            layout.style = OverlayStyle::Box;
        }
        Key {
            page_seq,
            layout: cam.as_ref().map(|_| layout),
            cam,
            mask,
        }
    }

    fn same_as(&self, o: &Key) -> bool {
        self.page_seq == o.page_seq
            && same(&self.cam, &o.cam)
            && same(&self.mask, &o.mask)
            && self.layout == o.layout
    }
}

/// Whether scaling `sw`x`sh` to `dw`x`dh` may use the hardware bilinear
/// sampler: upscales only. There fast_image_resize's convolution is plain
/// bilinear interpolation (kernel radius 1), so the sampler differs only by
/// rounding. Downscales widen the kernel; they keep the exact filter, which
/// now runs only when the PIP layer is rebuilt, over a rectangle smaller
/// than the source.
fn use_sampler(sw: u32, sh: u32, dw: u32, dh: u32) -> bool {
    dw >= sw && dh >= sh && (dw, dh) != (sw, sh)
}

/// What the cached PIP layer was built from (Arc identity, like [`Key`]).
#[derive(Clone)]
struct LayerKey {
    cam: Arc<Frame>,
    mask: Option<Arc<Mask>>,
    size: (u32, u32),
}

impl LayerKey {
    fn same_as(&self, o: &LayerKey) -> bool {
        Arc::ptr_eq(&self.cam, &o.cam) && same(&self.mask, &o.mask) && self.size == o.size
    }
}

/// The webcam x mask (premultiplied) at the PIP rectangle's size.
struct Layer {
    tex: Texture,
    key: LayerKey,
}

/// The PIP of one composite: where it goes and how its layer is built.
struct PipPlan {
    /// Destination rectangle in output pixels (x may be negative).
    rect: (i32, u32, u32, u32),
    build: PassConsts,
    cam_srv: Option<ID3D11ShaderResourceView>,
    mask_srv: Option<ID3D11ShaderResourceView>,
    key: LayerKey,
}

/// NV12 render targets at one size, owned by the producer (R8 Y + R8G8 UV).
struct Nv12Targets {
    y: Texture,
    uv: Texture,
}

impl Nv12Targets {
    fn new(gpu: &Gpu, w: u32, h: u32) -> Result<Nv12Targets> {
        let bind = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
        Ok(Nv12Targets {
            y: gpu.new_texture(w, h, DXGI_FORMAT_R8_UNORM, bind)?,
            uv: gpu.new_texture(w / 2, h / 2, DXGI_FORMAT_R8G8_UNORM, bind)?,
        })
    }
}

/// The admin preview: a small render target and two staging textures. A
/// preview rendered on one due tick is mapped on the next due tick, so the
/// CPU never waits for the GPU (the preview lags one preview interval).
struct Preview {
    rt: Texture,
    staging: [ID3D11Texture2D; 2],
    next: usize,
    /// Staging slot holding a preview not yet handed out.
    pending: Option<usize>,
    /// The composite the last rendered preview was made from.
    rendered: u64,
}

/// Composites on the GPU (D3D11) and produces NV12 output frames: see the
/// module docs. Requires `OutputFormat::Nv12` and an even output size.
pub struct GpuProducer {
    gpu: Gpu,
    page: PageInput,
    spec: OutputSpec,
    pipe: Pipeline,
    /// The composited BGRA frame at the output size.
    target: Texture,
    cam: Option<Uploaded<Frame>>,
    mask: Option<Uploaded<Mask>>,
    /// The cached PIP layer (R41) and how often it was built.
    layer: Option<Layer>,
    layer_builds: u64,
    out_nv12: Nv12Targets,
    /// Camera-size targets when the camera size differs from the output size.
    cam_nv12: Option<Nv12Targets>,
    camera_ok: bool,
    rb_out: Readback,
    rb_cam: Readback,
    preview: Preview,
    last_key: Option<Key>,
    /// The last output handed out (without `preview`).
    last: Option<Output>,
    /// Re-submits still needed to get the newest composite out of the
    /// readback rings when inputs stop changing.
    drain: u8,
    /// Counts composites (0 = none yet).
    composite: u64,
    /// Debug builds: `P2C_FAKE_DEVICE_LOSS` → (fail after this many
    /// `produce` calls, calls so far).
    #[cfg(debug_assertions)]
    fake_loss: Option<(u32, u32)>,
    /// GPU pass timing (benches only; reading it waits for the GPU).
    profiling: bool,
    prof: Profiler,
    last_profile: Option<Vec<(&'static str, f64)>>,
}

/// Debug builds only: `P2C_FAKE_DEVICE_LOSS=1` makes every `GpuProducer`
/// report device loss from its 91st `produce` call on (3 s at 30 fps), so the
/// recreate-then-fall-back path can be watched in a real run. Another number
/// `n` > 1 fails from call `n + 1` on.
#[cfg(debug_assertions)]
fn fake_loss_after() -> Option<u32> {
    let v = std::env::var("P2C_FAKE_DEVICE_LOSS").ok()?;
    match v.trim().parse::<u32>().ok()? {
        0 => None,
        1 => Some(90),
        n => Some(n),
    }
}

impl GpuProducer {
    pub fn new(gpu: Gpu, page: PageInput, spec: OutputSpec) -> Result<GpuProducer> {
        let (w, h) = (spec.width, spec.height);
        if spec.format != OutputFormat::Nv12 {
            bail!("the GPU compositor produces NV12 only");
        }
        if w == 0 || h == 0 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
            bail!("NV12 output needs an even, non-zero size (got {w}x{h})");
        }
        let pipe = Pipeline::new(&gpu)?;
        let bind = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
        let target = gpu.new_texture(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, bind)?;
        let out_nv12 = Nv12Targets::new(&gpu, w, h)?;
        let (cw, ch) = camera_size(w, h);
        let camera_ok = cw > 0 && ch > 0;
        let cam_nv12 = if camera_ok && (cw, ch) != (w, h) {
            Some(Nv12Targets::new(&gpu, cw, ch)?)
        } else {
            None
        };
        let (pw, ph) = preview_size(w, h);
        let preview = Preview {
            rt: gpu.new_texture(pw, ph, DXGI_FORMAT_B8G8R8A8_UNORM, bind)?,
            staging: [
                gpu.staging_like(pw, ph, DXGI_FORMAT_B8G8R8A8_UNORM)?,
                gpu.staging_like(pw, ph, DXGI_FORMAT_B8G8R8A8_UNORM)?,
            ],
            next: 0,
            pending: None,
            rendered: 0,
        };
        Ok(GpuProducer {
            gpu,
            page,
            spec,
            pipe,
            target,
            cam: None,
            mask: None,
            layer: None,
            layer_builds: 0,
            out_nv12,
            cam_nv12,
            camera_ok,
            rb_out: Readback::new(),
            rb_cam: Readback::new(),
            preview,
            last_key: None,
            last: None,
            drain: 0,
            composite: 0,
            #[cfg(debug_assertions)]
            fake_loss: fake_loss_after().map(|n| (n, 0)),
            profiling: false,
            prof: Profiler::new(),
            last_profile: None,
        })
    }

    /// The device this producer renders on.
    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// Time each GPU pass of every `produce` call (benches only: each call
    /// then waits for the GPU). Results via [`GpuProducer::take_profile`].
    #[doc(hidden)]
    pub fn set_profiling(&mut self, on: bool) {
        self.profiling = on;
    }

    /// GPU milliseconds per pass of the last profiled `produce` call.
    #[doc(hidden)]
    pub fn take_profile(&mut self) -> Option<Vec<(&'static str, f64)>> {
        self.last_profile.take()
    }

    /// How many times the PIP layer was built (tests: R41 caching).
    #[doc(hidden)]
    pub fn layer_builds(&self) -> u64 {
        self.layer_builds
    }

    /// A profiling timestamp after the GPU work queued so far.
    fn mark(&mut self, name: &'static str) -> Result<()> {
        if self.profiling {
            self.prof.mark(&self.gpu, name)?;
        }
        Ok(())
    }

    /// Composite `inputs` and read the BGRA intermediate back synchronously.
    /// For parity tests only: it does not touch change detection or the
    /// readback rings.
    #[doc(hidden)]
    pub fn render_bgra_for_test(&mut self, inputs: &TickInputs) -> Result<Frame> {
        let key = Key::of(self.page.seq(), inputs);
        self.composite(&key)?;
        self.gpu.read_bgra(&self.target)
    }

    /// Upload the webcam frame / mask if their Arc changed.
    fn upload(&mut self, key: &Key) -> Result<()> {
        if let Some(cam) = &key.cam {
            match &self.cam {
                Some(u) if Arc::ptr_eq(&u.src, cam) => {}
                Some(u) if (u.tex.width(), u.tex.height()) == (cam.width, cam.height) => {
                    self.gpu.update_bgra(&u.tex, cam)?;
                    let tex = u.tex.clone();
                    self.cam = Some(Uploaded {
                        src: cam.clone(),
                        tex,
                    });
                }
                _ => {
                    self.cam = Some(Uploaded {
                        src: cam.clone(),
                        tex: self.gpu.upload_bgra(cam)?,
                    });
                }
            }
        }
        if let Some(m) = &key.mask {
            match &self.mask {
                Some(u) if Arc::ptr_eq(&u.src, m) => {}
                Some(u) if (u.tex.width(), u.tex.height()) == (m.width, m.height) => {
                    self.gpu.update_r8(&u.tex, &m.data)?;
                    let tex = u.tex.clone();
                    self.mask = Some(Uploaded {
                        src: m.clone(),
                        tex,
                    });
                }
                _ => {
                    self.mask = Some(Uploaded {
                        src: m.clone(),
                        tex: self.gpu.upload_r8(m.width, m.height, &m.data)?,
                    });
                }
            }
        }
        Ok(())
    }

    /// The PIP draw for `key`: its rectangle, and the layer-build constants
    /// with the webcam and mask views. `None` without a visible PIP.
    fn pip_plan(&self, key: &Key) -> Option<PipPlan> {
        let (ow, oh) = (self.spec.width, self.spec.height);
        let (cam, layout, up) = (key.cam.as_ref()?, key.layout.as_ref()?, self.cam.as_ref()?);
        let (x, y, w, h) = pip_rect(layout, cam.width, cam.height, ow, oh)?;
        if w == 0 || h == 0 {
            return None;
        }
        let mut c = PassConsts {
            rect: [0.0, 0.0, w as f32, h as f32],
            src_size: [cam.width as f32, cam.height as f32],
            mode: MODE_OPAQUE,
            cam_linear: u32::from(use_sampler(cam.width, cam.height, w, h)),
            ..Default::default()
        };
        let mut mask_srv = None;
        if let (Some(m), Some(mu)) = (&key.mask, &self.mask) {
            c.mode = MODE_CUTOUT;
            c.mask_size = [m.width as f32, m.height as f32];
            c.mask_linear = u32::from(use_sampler(m.width, m.height, w, h));
            let e = m.edge;
            c.edge_full = u32::from(e == EdgeRange::FULL || e.hi <= e.lo);
            c.edge_lo = e.lo as f32;
            c.edge_hi = e.hi as f32;
            mask_srv = mu.tex.srv().cloned();
        }
        Some(PipPlan {
            rect: (x, y, w, h),
            build: c,
            cam_srv: up.tex.srv().cloned(),
            mask_srv,
            key: LayerKey {
                cam: cam.clone(),
                mask: key.mask.clone(),
                size: (w, h),
            },
        })
    }

    /// Render webcam x mask into the layer texture (only when its inputs or
    /// size changed since the last build).
    fn build_layer(&mut self, plan: &PipPlan) -> Result<()> {
        if self
            .layer
            .as_ref()
            .is_some_and(|l| l.key.same_as(&plan.key))
        {
            return Ok(());
        }
        let (w, h) = plan.key.size;
        let tex = match self.layer.take() {
            Some(l) if (l.tex.width(), l.tex.height()) == (w, h) => l.tex,
            _ => {
                let bind = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
                self.gpu
                    .new_texture(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, bind)?
            }
        };
        let rtv = tex.rtv().cloned().context("layer has no RTV")?;
        let ctx = self.gpu.context();
        let s = self.gpu.shaders();
        let c = plan.build;
        {
            let _lock = self.gpu.lock();
            // SAFETY: resources of this device; state sequence under the lock.
            unsafe {
                let cb = &self.pipe.cbs[1];
                ctx.UpdateSubresource(cb, 0, None, &c as *const _ as _, 0, 0);
                ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                ctx.IASetInputLayout(None);
                ctx.VSSetShader(&s.vs_fullscreen, None);
                ctx.PSSetShader(&self.pipe.ps, None);
                ctx.OMSetBlendState(None, None, 0xffff_ffff);
                ctx.RSSetState(None);
                ctx.OMSetRenderTargets(Some(&[Some(rtv)]), None);
                ctx.PSSetConstantBuffers(0, Some(&[Some(cb.clone())]));
                ctx.PSSetSamplers(0, Some(&[Some(s.linear_clamp.clone())]));
                ctx.PSSetShaderResources(0, Some(&[plan.cam_srv.clone(), plan.mask_srv.clone()]));
                ctx.RSSetViewports(Some(&[viewport(0.0, 0.0, w as f32, h as f32)]));
                ctx.Draw(3, 0);
                ctx.OMSetRenderTargets(None, None);
                ctx.PSSetShaderResources(0, Some(&[None, None]));
            }
        }
        self.layer = Some(Layer {
            tex,
            key: plan.key.clone(),
        });
        self.layer_builds += 1;
        Ok(())
    }

    /// Draw page + PIP into `self.target`.
    fn composite(&mut self, key: &Key) -> Result<()> {
        self.upload(key)?;
        self.mark("upload")?;
        let pip = self.pip_plan(key);
        if let Some(plan) = &pip {
            self.build_layer(plan)?;
        }
        self.mark("layer")?;
        // Take the page texture before locking the context: PageInput's
        // mutex must never be taken while holding `gpu.lock()` (R22).
        let page = (key.page_seq > 0).then(|| self.page.texture());
        let (ow, oh) = (self.spec.width, self.spec.height);

        let page_draw = page.as_ref().and_then(|t| {
            let (x, y, w, h) = fit_rect(t.width(), t.height(), ow, oh);
            (w > 0 && h > 0).then(|| {
                let c = PassConsts {
                    rect: [x as f32, y as f32, w as f32, h as f32],
                    src_size: [t.width() as f32, t.height() as f32],
                    mode: MODE_OPAQUE,
                    ..Default::default()
                };
                (t, c)
            })
        });
        let layer_draw = pip.as_ref().and_then(|plan| {
            let l = self.layer.as_ref()?;
            let (x, y, w, h) = plan.rect;
            let c = PassConsts {
                rect: [x as f32, y as f32, w as f32, h as f32],
                ..Default::default()
            };
            Some((l.tex.srv().cloned(), c))
        });

        let rtv = self.target.rtv().cloned().context("target has no RTV")?;
        let ctx = self.gpu.context();
        let s = self.gpu.shaders();
        let _lock = self.gpu.lock();
        // SAFETY: all resources belong to this device; the lock keeps other
        // threads from changing pipeline state in between.
        unsafe {
            // An unscaled page covering the output is copied, no clear.
            let covers = page_draw
                .as_ref()
                .is_some_and(|(_, c)| c.rect == [0.0, 0.0, ow as f32, oh as f32]);
            if !covers {
                ctx.ClearRenderTargetView(&rtv, &[0.0, 0.0, 0.0, 1.0]);
            }
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.IASetInputLayout(None);
            ctx.VSSetShader(&s.vs_fullscreen, None);
            ctx.RSSetState(None);
            if let Some((t, c)) = &page_draw {
                if c.rect[2..] == c.src_size {
                    // Same size: a plain copy into place (letterbox or not).
                    ctx.CopySubresourceRegion(
                        self.target.raw(),
                        0,
                        c.rect[0] as u32,
                        c.rect[1] as u32,
                        0,
                        t.raw(),
                        0,
                        None,
                    );
                } else {
                    let cb = &self.pipe.cbs[0];
                    ctx.UpdateSubresource(cb, 0, None, c as *const _ as _, 0, 0);
                    ctx.PSSetShader(&self.pipe.ps, None);
                    ctx.OMSetBlendState(None, None, 0xffff_ffff);
                    ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
                    ctx.PSSetConstantBuffers(0, Some(&[Some(cb.clone())]));
                    ctx.PSSetShaderResources(0, Some(&[t.srv().cloned(), None]));
                    ctx.RSSetViewports(Some(&[viewport(
                        c.rect[0], c.rect[1], c.rect[2], c.rect[3],
                    )]));
                    ctx.Draw(3, 0);
                }
            }
            if self.profiling {
                self.prof.mark(&self.gpu, "page")?;
            }
            if let Some((srv, c)) = layer_draw {
                let cb = &self.pipe.cbs[2];
                ctx.UpdateSubresource(cb, 0, None, &c as *const _ as _, 0, 0);
                ctx.PSSetShader(&self.pipe.ps_layer, None);
                ctx.OMSetBlendState(&self.pipe.blend, None, 0xffff_ffff);
                ctx.OMSetRenderTargets(Some(&[Some(rtv)]), None);
                ctx.PSSetConstantBuffers(0, Some(&[Some(cb.clone())]));
                ctx.PSSetShaderResources(0, Some(&[srv, None]));
                ctx.RSSetViewports(Some(&[viewport(
                    c.rect[0], c.rect[1], c.rect[2], c.rect[3],
                )]));
                ctx.Draw(3, 0);
            }
            if self.profiling {
                self.prof.mark(&self.gpu, "pip")?;
            }
            // Unbind so the target can be sampled and the inputs updated.
            ctx.OMSetRenderTargets(None, None);
            ctx.PSSetShaderResources(0, Some(&[None, None]));
            ctx.OMSetBlendState(None, None, 0xffff_ffff);
        }
        Ok(())
    }
    /// Convert the composite to NV12 (output size and, if different, camera size).
    fn convert(&self) -> Result<()> {
        bgra_to_nv12_into(&self.gpu, &self.target, &self.out_nv12.y, &self.out_nv12.uv)?;
        if let Some(t) = &self.cam_nv12 {
            bgra_to_nv12_into(&self.gpu, &self.target, &t.y, &t.uv)?;
        }
        Ok(())
    }

    /// Queue the NV12 targets' readback and take the frame from two submits ago.
    fn submit_and_take(&mut self) -> Result<Option<Output>> {
        self.rb_out
            .submit(&self.gpu, &self.out_nv12.y, &self.out_nv12.uv)?;
        if let Some(t) = &self.cam_nv12 {
            self.rb_cam.submit(&self.gpu, &t.y, &t.uv)?;
        }
        let Some(nv12) = self.rb_out.take()? else {
            return Ok(None);
        };
        let nv12 = Arc::new(nv12);
        let camera: Option<Arc<Nv12Frame>> = match (&self.cam_nv12, self.camera_ok) {
            (_, false) => None,
            (None, true) => Some(nv12.clone()),
            (Some(_), true) => self.rb_cam.take()?.map(Arc::new),
        };
        Ok(Some(Output {
            nv12: Some(nv12),
            camera,
            ..Output::default()
        }))
    }

    /// On a due tick: hand out the preview staged on an earlier due tick (if
    /// any), and stage one of the current composite if it hasn't been.
    fn preview_step(&mut self) -> Result<Option<Arc<Frame>>> {
        let mut ready = None;
        if let Some(i) = self.preview.pending.take() {
            let st = &self.preview.staging[i];
            let (w, h) = (self.preview.rt.width(), self.preview.rt.height());
            let mut data = Vec::with_capacity(w as usize * h as usize * 4);
            self.gpu.read_staging(st, &mut data)?;
            ready = Some(Arc::new(Frame::from_bgra(w, h, data)?));
        }
        if self.composite > 0 && self.preview.rendered != self.composite {
            self.render_preview()?;
        }
        Ok(ready)
    }

    /// Scale the composite into the preview target and queue its copy.
    fn render_preview(&mut self) -> Result<()> {
        let p = &mut self.preview;
        let (pw, ph) = (p.rt.width(), p.rt.height());
        let c = PassConsts {
            rect: [0.0, 0.0, pw as f32, ph as f32],
            src_size: [self.spec.width as f32, self.spec.height as f32],
            mode: MODE_OPAQUE,
            ..Default::default()
        };
        // Up to a 2x downscale the bilinear sampler (one tap) looks the
        // same as the exact filter (a 2x2 box at exactly 2x); beyond, keep
        // the exact filter so the preview doesn't alias.
        let shader = if pw * 2 >= self.spec.width && ph * 2 >= self.spec.height {
            &self.pipe.ps_sample
        } else {
            &self.pipe.ps
        };
        let rtv = p.rt.rtv().cloned().context("preview has no RTV")?;
        let srv = self.target.srv().cloned();
        let i = p.next;
        let ctx = self.gpu.context();
        let s = self.gpu.shaders();
        let _lock = self.gpu.lock();
        // SAFETY: resources of this device; state sequence under the lock.
        unsafe {
            let cb = &self.pipe.cbs[3];
            ctx.UpdateSubresource(cb, 0, None, &c as *const _ as _, 0, 0);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.IASetInputLayout(None);
            ctx.VSSetShader(&s.vs_fullscreen, None);
            ctx.PSSetShader(shader, None);
            ctx.PSSetSamplers(0, Some(&[Some(s.linear_clamp.clone())]));
            ctx.OMSetBlendState(None, None, 0xffff_ffff);
            ctx.RSSetState(None);
            ctx.OMSetRenderTargets(Some(&[Some(rtv)]), None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(cb.clone())]));
            ctx.PSSetShaderResources(0, Some(&[srv, None]));
            ctx.RSSetViewports(Some(&[viewport(0.0, 0.0, pw as f32, ph as f32)]));
            ctx.Draw(3, 0);
            ctx.OMSetRenderTargets(None, None);
            ctx.PSSetShaderResources(0, Some(&[None, None]));
            ctx.CopyResource(&p.staging[i], p.rt.raw());
        }
        p.pending = Some(i);
        p.next = 1 - i;
        p.rendered = self.composite;
        Ok(())
    }
}

/// The preview size: the output fitted into 960x540 (never upscaled).
fn preview_size(w: u32, h: u32) -> (u32, u32) {
    if w <= PREVIEW_MAX.0 && h <= PREVIEW_MAX.1 {
        return (w, h);
    }
    let (_, _, pw, ph) = fit_rect(w, h, PREVIEW_MAX.0, PREVIEW_MAX.1);
    (pw.max(1), ph.max(1))
}

impl OutputProducer for GpuProducer {
    fn produce(&mut self, inputs: &TickInputs, preview_due: bool) -> Result<Option<Output>> {
        #[cfg(debug_assertions)]
        if let Some((after, calls)) = &mut self.fake_loss {
            *calls += 1;
            if *calls > *after {
                let e = windows::core::Error::from(
                    windows::Win32::Graphics::Dxgi::DXGI_ERROR_DEVICE_REMOVED,
                );
                return Err(
                    anyhow::Error::from(e).context("fake device loss (P2C_FAKE_DEVICE_LOSS)")
                );
            }
        }
        if self.profiling {
            self.prof.begin(&self.gpu)?;
        }
        let r = self.produce_inner(inputs, preview_due);
        if self.profiling {
            self.last_profile = self.prof.finish(&self.gpu)?;
        }
        if r.is_err() {
            // The change key may have been set before the failing step: draw
            // again on the next tick.
            self.last_key = None;
            self.drain = 0;
        }
        r
    }

    fn name(&self) -> String {
        "GPU".into()
    }

    fn pipeline_frames(&self) -> u32 {
        2
    }
}

impl GpuProducer {
    fn produce_inner(&mut self, inputs: &TickInputs, preview_due: bool) -> Result<Option<Output>> {
        let key = Key::of(self.page.seq(), inputs);
        let changed = !self.last_key.as_ref().is_some_and(|k| k.same_as(&key));
        let fresh = if changed {
            self.composite(&key)?;
            self.composite += 1;
            self.last_key = Some(key);
            self.convert()?;
            self.mark("nv12")?;
            // Two more submits bring this composite out of the N-2 rings.
            self.drain = 2;
            self.submit_and_take()?
        } else if self.drain > 0 {
            // Unchanged: re-submit the same NV12 planes to drain the ring.
            self.drain -= 1;
            self.submit_and_take()?
        } else {
            None
        };
        self.mark("readback")?;
        let got_fresh = fresh.is_some();
        if let Some(o) = fresh {
            self.last = Some(o);
        }
        let Some(last) = &self.last else {
            return Ok(None);
        };
        let last = last.clone();
        let preview = if preview_due {
            let p = self.preview_step()?;
            self.mark("preview")?;
            p
        } else {
            None
        };
        if !got_fresh && preview.is_none() {
            return Ok(None);
        }
        Ok(Some(Output { preview, ..last }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::compile;

    #[test]
    fn compose_shader_compiles() {
        for e in ["ps_compose", "ps_layer", "ps_sample"] {
            compile(COMPOSE_HLSL, "compose.hlsl", e, "ps_5_0").unwrap();
        }
    }

    #[test]
    fn pass_constants_are_four_registers() {
        assert_eq!(std::mem::size_of::<PassConsts>(), 64);
    }

    #[test]
    fn sampler_only_for_upscales() {
        assert!(use_sampler(1280, 720, 1920, 1080)); // cut-out full webcam
        assert!(use_sampler(640, 360, 1920, 1080)); // its mask
        assert!(!use_sampler(1280, 720, 1280, 720)); // same size: exact Load
        assert!(!use_sampler(1280, 720, 480, 270)); // Box at 0.25: exact filter
        assert!(!use_sampler(1280, 720, 1300, 700)); // mixed: exact filter
    }

    #[test]
    fn preview_fits_960x540() {
        assert_eq!(preview_size(1920, 1080), (960, 540));
        assert_eq!(preview_size(3840, 2160), (960, 540));
        assert_eq!(preview_size(640, 360), (640, 360));
        assert_eq!(preview_size(1440, 1080), (720, 540));
    }
}
