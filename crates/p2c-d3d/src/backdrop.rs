//! Backdrops and refined edges on the GPU (mirrors `p2c_compositor`'s
//! `BlurCache`, box image and `GuidedScratch`): the blur passes
//! (blur.hlsl), the guided-filter passes (guided.hlsl) and their cached
//! textures. What to draw is decided by `p2c_compositor::plan_compose`, the
//! same function the CPU compositor uses; [`crate::compose`] draws it.
//!
//! The shaders are compiled once per process on a background thread started
//! when the first `GpuProducer` is created ([`compile_in_background`]), so
//! turning a backdrop on later never stalls the output. The bytecode doesn't
//! depend on the device; each producer only creates its shader objects.
//!
//! Textures that are only read with `Load` at explicit sizes grow and are
//! never shrunk, so dragging the PIP's size doesn't reallocate them every
//! tick; textures read through the bilinear sampler keep their exact size.

use std::sync::{Arc, Once, OnceLock};

use anyhow::{ensure, Context, Result};
use p2c_compositor::{
    blur_kernel, blur_low_size, cover_crop, refined_size, GUIDED_EPS, GUIDED_RADIUS,
    GUIDED_SUBSAMPLE, REFINED_MAX_W,
};
use p2c_core::Frame;
use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Buffer, ID3D11Device, ID3D11PixelShader, ID3D11ShaderResourceView,
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BUFFER_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R32G32B32A32_FLOAT, DXGI_FORMAT_R8_UNORM,
};

use crate::compose::{use_sampler, viewport, PassConsts, COMPOSE_HLSL};
use crate::device::{Gpu, Texture};
use crate::shaders::compile;

pub(crate) const BLUR_HLSL: &str = include_str!("hlsl/blur.hlsl");
pub(crate) const GUIDED_HLSL: &str = include_str!("hlsl/guided.hlsl");

/// Most Gaussian taps the blur constants hold (radius 10 at the 160 px cap).
const MAX_TAPS: usize = 32;

/// `cbuffer BlurConsts` (b1) of blur.hlsl, 176 bytes.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct BlurConsts {
    pub(crate) crop: [f32; 4],
    pub(crate) fill: [f32; 4],
    pub(crate) radius: u32,
    pub(crate) pad: [u32; 3],
    pub(crate) k: [f32; MAX_TAPS],
}

/// `cbuffer GuidedConsts` (b1) of guided.hlsl, 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct GuidedConsts {
    g_size: [f32; 2],
    low_size: [f32; 2],
    mask_size: [f32; 2],
    radius: u32,
    eps: f32,
}

/// Entry points, in [`Blobs`] order: (blur file?, entry).
const ENTRIES: [(bool, &str); 10] = [
    (true, "ps_cover"),
    (true, "ps_blur_h"),
    (true, "ps_blur_v"),
    (true, "ps_fill"),
    (false, "ps_guide_luma"),
    (false, "ps_guide_stats"),
    (false, "ps_box_h"),
    (false, "ps_box_v_ab"),
    (false, "ps_box_v"),
    (false, "ps_guided_out"),
];

/// Compiled bytecode of [`ENTRIES`] (device independent).
pub(crate) struct Blobs {
    code: Vec<Vec<u8>>,
    /// How long compiling took (for the log).
    pub(crate) ms: f64,
}

/// Compile every backdrop entry point (tens to hundreds of ms).
pub(crate) fn compile_blobs() -> Result<Blobs> {
    let t = std::time::Instant::now();
    let blur = format!("{COMPOSE_HLSL}\n{BLUR_HLSL}");
    let guided = format!("{COMPOSE_HLSL}\n{GUIDED_HLSL}");
    let code = ENTRIES
        .iter()
        .map(|(is_blur, e)| {
            if *is_blur {
                compile(&blur, "blur.hlsl", e, "ps_5_0")
            } else {
                compile(&guided, "guided.hlsl", e, "ps_5_0")
            }
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Blobs {
        code,
        ms: t.elapsed().as_secs_f64() * 1e3,
    })
}

static BLOBS: OnceLock<Result<Blobs, String>> = OnceLock::new();
static STARTED: Once = Once::new();

fn init_blobs() -> Result<Blobs, String> {
    let r = compile_blobs().map_err(|e| format!("{e:#}"));
    match &r {
        Ok(b) => tracing::info!(ms = b.ms, "GPU backdrop shaders compiled"),
        Err(e) => tracing::warn!(
            error = %e,
            "GPU backdrop shaders failed to compile; backgrounds and refined edges are off on the GPU"
        ),
    }
    r
}

/// Start compiling the backdrop shaders on a background thread (once per
/// process; later calls do nothing).
pub(crate) fn compile_in_background() {
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new()
            .name("p2c-shader-compile".into())
            .spawn(|| {
                BLOBS.get_or_init(init_blobs);
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "can't start the shader compile thread; compiling on first use");
        }
    });
}

/// The compiled bytecode, waiting for the background compile if it is still
/// running (it starts with the first producer, so by the time a backdrop is
/// switched on it is normally long done).
pub(crate) fn blobs() -> &'static Result<Blobs, String> {
    BLOBS.get_or_init(init_blobs)
}

fn cbuffer(device: &ID3D11Device, size: usize) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: size as u32,
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
}

fn pixel_shader(device: &ID3D11Device, code: &[u8], entry: &str) -> Result<ID3D11PixelShader> {
    let mut s = None;
    // SAFETY: valid bytecode slice and out-pointer.
    unsafe { device.CreatePixelShader(code, None, Some(&mut s)) }
        .with_context(|| format!("CreatePixelShader {entry}"))?;
    s.context("CreatePixelShader returned nothing")
}

/// Backdrop / refined-edge shader objects of one device and their constant
/// buffers.
pub(crate) struct BackdropShaders {
    pub(crate) cover: ID3D11PixelShader,
    blur_h: ID3D11PixelShader,
    blur_v: ID3D11PixelShader,
    pub(crate) fill: ID3D11PixelShader,
    luma: ID3D11PixelShader,
    stats: ID3D11PixelShader,
    box_h: ID3D11PixelShader,
    box_v_ab: ID3D11PixelShader,
    box_v: ID3D11PixelShader,
    guided_out: ID3D11PixelShader,
    /// PassConsts (b0) of the passes run here.
    cb_pass: ID3D11Buffer,
    /// BlurConsts (b1); also the fill colour of the box draw.
    pub(crate) cb_blur: ID3D11Buffer,
    cb_guided: ID3D11Buffer,
}

impl BackdropShaders {
    pub(crate) fn new(device: &ID3D11Device, blobs: &Blobs) -> Result<BackdropShaders> {
        let s = |i: usize| pixel_shader(device, &blobs.code[i], ENTRIES[i].1);
        Ok(BackdropShaders {
            cover: s(0)?,
            blur_h: s(1)?,
            blur_v: s(2)?,
            fill: s(3)?,
            luma: s(4)?,
            stats: s(5)?,
            box_h: s(6)?,
            box_v_ab: s(7)?,
            box_v: s(8)?,
            guided_out: s(9)?,
            cb_pass: cbuffer(device, std::mem::size_of::<PassConsts>())?,
            cb_blur: cbuffer(device, std::mem::size_of::<BlurConsts>())?,
            cb_guided: cbuffer(device, std::mem::size_of::<GuidedConsts>())?,
        })
    }
}

/// Upload `v` into the constant buffer `cb`.
fn upload<T>(gpu: &Gpu, cb: &ID3D11Buffer, v: &T) {
    let _lock = gpu.lock();
    // SAFETY: `cb` was created with size_of::<T>() bytes; `v` outlives the call.
    unsafe {
        gpu.context()
            .UpdateSubresource(cb, 0, None, v as *const T as _, 0, 0)
    };
}

/// One fullscreen-triangle pass into the top-left `w` x `h` of `target`,
/// no blending, `cb1` (already uploaded) bound at b1. Takes the context
/// lock itself: never call it while holding it.
#[allow(clippy::too_many_arguments)]
fn pass(
    gpu: &Gpu,
    sh: &BackdropShaders,
    shader: &ID3D11PixelShader,
    target: &Texture,
    (w, h): (u32, u32),
    c0: &PassConsts,
    cb1: Option<&ID3D11Buffer>,
    srvs: [Option<ID3D11ShaderResourceView>; 3],
) -> Result<()> {
    let rtv = target.rtv().cloned().context("pass target has no RTV")?;
    let ctx = gpu.context();
    let s = gpu.shaders();
    let _lock = gpu.lock();
    // SAFETY: resources of this device; state sequence under the lock.
    unsafe {
        ctx.UpdateSubresource(&sh.cb_pass, 0, None, c0 as *const PassConsts as _, 0, 0);
        ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ctx.IASetInputLayout(None);
        ctx.VSSetShader(&s.vs_fullscreen, None);
        ctx.PSSetShader(shader, None);
        ctx.OMSetBlendState(None, None, 0xffff_ffff);
        ctx.RSSetState(None);
        ctx.OMSetRenderTargets(Some(&[Some(rtv)]), None);
        ctx.PSSetConstantBuffers(0, Some(&[Some(sh.cb_pass.clone()), cb1.cloned()]));
        ctx.PSSetSamplers(0, Some(&[Some(s.linear_clamp.clone())]));
        ctx.PSSetShaderResources(0, Some(&srvs));
        ctx.RSSetViewports(Some(&[viewport(0.0, 0.0, w as f32, h as f32)]));
        ctx.Draw(3, 0);
        ctx.OMSetRenderTargets(None, None);
        ctx.PSSetShaderResources(0, Some(&[None, None, None]));
    }
    Ok(())
}

/// Make `slot` a render target of at least `w` x `h` (`exact`: exactly).
/// Growing keeps the larger of the old and new sizes per axis, so a PIP
/// resize drag reallocates only while it grows past the largest size yet.
fn ensure_rt(
    slot: &mut Option<Texture>,
    gpu: &Gpu,
    (w, h): (u32, u32),
    format: DXGI_FORMAT,
    exact: bool,
) -> Result<()> {
    let fits = slot.as_ref().is_some_and(|t| {
        let (tw, th) = (t.width(), t.height());
        if exact {
            (tw, th) == (w, h)
        } else {
            tw >= w && th >= h
        }
    });
    if !fits {
        let (aw, ah) = match (exact, slot.as_ref()) {
            (false, Some(t)) => (t.width().max(w), t.height().max(h)),
            _ => (w, h),
        };
        let bind = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
        *slot = Some(gpu.new_texture(aw, ah, format, bind)?);
    }
    Ok(())
}

/// Scale `src` (its crop `crop`, in source pixels) into the top-left `size`
/// of `dst` with the exact filter, opaque.
fn cover_pass(
    gpu: &Gpu,
    sh: &BackdropShaders,
    src: &Texture,
    crop: (f64, f64, f64, f64),
    dst: &Texture,
    size: (u32, u32),
) -> Result<()> {
    let c0 = PassConsts {
        rect: [0.0, 0.0, size.0 as f32, size.1 as f32],
        src_size: [src.width() as f32, src.height() as f32],
        ..Default::default()
    };
    let c1 = BlurConsts {
        crop: [crop.0 as f32, crop.1 as f32, crop.2 as f32, crop.3 as f32],
        ..Default::default()
    };
    upload(gpu, &sh.cb_blur, &c1);
    pass(
        gpu,
        sh,
        &sh.cover,
        dst,
        size,
        &c0,
        Some(&sh.cb_blur),
        [src.srv().cloned(), None, None],
    )
}

/// A blurred webcam at reduced size (`BlurCache` on the GPU): drawn
/// upscaled with the bilinear sampler. Rebuilt only for another webcam
/// frame (Arc identity), another drawn size or another fit.
pub(crate) struct GpuBlur {
    key: Option<(Arc<Frame>, (u32, u32), bool)>,
    /// Scaled-down webcam and the horizontal pass (`Load` only: grow-only).
    small: Option<Texture>,
    tmp: Option<Texture>,
    /// The result, sampled when drawn: exactly the reduced size.
    out: Option<Texture>,
    pub(crate) builds: u64,
}

impl GpuBlur {
    pub(crate) fn new() -> GpuBlur {
        GpuBlur {
            key: None,
            small: None,
            tmp: None,
            out: None,
            builds: 0,
        }
    }

    /// The blurred `cam` for a `w` x `h` destination (stretched, or
    /// cover-cropped when `cover`), at reduced size.
    pub(crate) fn get(
        &mut self,
        gpu: &Gpu,
        sh: &BackdropShaders,
        cam: &Arc<Frame>,
        cam_tex: &Texture,
        (w, h): (u32, u32),
        cover: bool,
    ) -> Result<&Texture> {
        let fresh =
            self.key.as_ref().is_some_and(|(c, size, cv)| {
                Arc::ptr_eq(c, cam) && *size == (w, h) && *cv == cover
            }) && self.out.is_some();
        if !fresh {
            self.key = None;
            self.build(gpu, sh, cam, cam_tex, (w, h), cover)?;
            self.key = Some((cam.clone(), (w, h), cover));
            self.builds += 1;
        }
        self.out.as_ref().context("blur not built")
    }

    fn build(
        &mut self,
        gpu: &Gpu,
        sh: &BackdropShaders,
        cam: &Frame,
        cam_tex: &Texture,
        (w, h): (u32, u32),
        cover: bool,
    ) -> Result<()> {
        let low = blur_low_size(w, h);
        ensure_rt(&mut self.small, gpu, low, DXGI_FORMAT_B8G8R8A8_UNORM, false)?;
        ensure_rt(
            &mut self.tmp,
            gpu,
            low,
            DXGI_FORMAT_R32G32B32A32_FLOAT,
            false,
        )?;
        ensure_rt(&mut self.out, gpu, low, DXGI_FORMAT_B8G8R8A8_UNORM, true)?;
        let (small, tmp, out) = (
            self.small.as_ref().context("blur small")?,
            self.tmp.as_ref().context("blur tmp")?,
            self.out.as_ref().context("blur out")?,
        );
        let crop = if cover {
            cover_crop(cam.width, cam.height, low.0, low.1)
        } else {
            (0.0, 0.0, cam.width as f64, cam.height as f64)
        };
        cover_pass(gpu, sh, cam_tex, crop, small, low)?;
        let kernel = blur_kernel(low.0);
        ensure!(
            kernel.len() <= MAX_TAPS,
            "blur kernel of {} taps",
            kernel.len()
        );
        let mut c1 = BlurConsts {
            radius: (kernel.len() / 2) as u32,
            ..Default::default()
        };
        c1.k[..kernel.len()].copy_from_slice(&kernel);
        upload(gpu, &sh.cb_blur, &c1);
        let c0 = PassConsts {
            rect: [0.0, 0.0, low.0 as f32, low.1 as f32],
            ..Default::default()
        };
        let cb = Some(&sh.cb_blur);
        let srv = |t: &Texture| [t.srv().cloned(), None, None];
        pass(gpu, sh, &sh.blur_h, tmp, low, &c0, cb, srv(small))?;
        pass(gpu, sh, &sh.blur_v, out, low, &c0, cb, srv(tmp))
    }
}

/// The box image: uploaded once per image (Arc identity), cover-scaled to
/// the box once per image and size.
pub(crate) struct BoxImage {
    src: Arc<Frame>,
    /// `None` for an image that can't be shown (empty, malformed, too big).
    tex: Option<Texture>,
    /// The size scaled into the top-left of `scaled` (drawn 1:1 with `Load`,
    /// so the texture only grows).
    size: Option<(u32, u32)>,
    scaled: Option<Texture>,
    pub(crate) uploads: u64,
}

/// Largest texture side D3D11 guarantees (feature level 11).
const MAX_SIDE: u32 = 16384;

impl BoxImage {
    /// The image cover-scaled to `w` x `h` (opaque) in the top-left of the
    /// returned texture, or `None` (draw black) for an image that can't be
    /// shown.
    pub(crate) fn scaled<'a>(
        slot: &'a mut Option<BoxImage>,
        gpu: &Gpu,
        sh: &BackdropShaders,
        img: &Arc<Frame>,
        (w, h): (u32, u32),
    ) -> Result<Option<&'a Texture>> {
        if !slot.as_ref().is_some_and(|b| Arc::ptr_eq(&b.src, img)) {
            let (uploads, scaled) = slot.take().map_or((0, None), |b| (b.uploads, b.scaled));
            let valid = img.width > 0
                && img.height > 0
                && img.width <= MAX_SIDE
                && img.height <= MAX_SIDE
                && img.data.len() == img.width as usize * img.height as usize * 4;
            let tex = if valid {
                Some(gpu.upload_bgra(img)?)
            } else {
                None
            };
            *slot = Some(BoxImage {
                src: img.clone(),
                tex,
                size: None,
                scaled,
                uploads: uploads + 1,
            });
        }
        let b = slot.as_mut().context("box image")?;
        let Some(tex) = &b.tex else {
            return Ok(None);
        };
        if b.size != Some((w, h)) {
            b.size = None;
            ensure_rt(
                &mut b.scaled,
                gpu,
                (w, h),
                DXGI_FORMAT_B8G8R8A8_UNORM,
                false,
            )?;
            let dst = b.scaled.as_ref().context("box image target")?;
            let crop = cover_crop(img.width, img.height, w, h);
            cover_pass(gpu, sh, tex, crop, dst, (w, h))?;
            b.size = Some((w, h));
        }
        Ok(b.scaled.as_ref())
    }
}

/// Textures of the guided filter. All but `out` are read with `Load` at
/// explicit sizes and only grow; `out` grows too unless the matte is scaled
/// up into the PIP (PIP wider than 960), where the sampler needs it exact.
#[derive(Default)]
pub(crate) struct Guided {
    luma: Option<Texture>,
    stats: Option<Texture>,
    tmp: Option<Texture>,
    ab: Option<Texture>,
    out: Option<Texture>,
}

impl Guided {
    /// The refined matte (R8) of a `w` x `h` PIP, in the top-left
    /// `refined_size(w, h)` of the returned texture: the model mask
    /// `mask_tex` upsampled with the webcam `cam_tex` as guide.
    pub(crate) fn run<'a>(
        &'a mut self,
        gpu: &Gpu,
        sh: &BackdropShaders,
        cam_tex: &Texture,
        mask_tex: &Texture,
        (w, h): (u32, u32),
    ) -> Result<&'a Texture> {
        let g = refined_size(w, h);
        let s = GUIDED_SUBSAMPLE as u32;
        let low = (g.0.div_ceil(s), g.1.div_ceil(s));
        let f32x4 = DXGI_FORMAT_R32G32B32A32_FLOAT;
        ensure_rt(&mut self.luma, gpu, g, DXGI_FORMAT_R8_UNORM, false)?;
        ensure_rt(&mut self.stats, gpu, low, f32x4, false)?;
        ensure_rt(&mut self.tmp, gpu, low, f32x4, false)?;
        ensure_rt(&mut self.ab, gpu, low, f32x4, false)?;
        let scaled_up = w > REFINED_MAX_W;
        ensure_rt(&mut self.out, gpu, g, DXGI_FORMAT_R8_UNORM, scaled_up)?;
        let t = |t: &'a Option<Texture>| t.as_ref().context("guided target");
        let (luma, stats, tmp, ab, out) = (
            t(&self.luma)?,
            t(&self.stats)?,
            t(&self.tmp)?,
            t(&self.ab)?,
            t(&self.out)?,
        );
        let gc = GuidedConsts {
            g_size: [g.0 as f32, g.1 as f32],
            low_size: [low.0 as f32, low.1 as f32],
            mask_size: [mask_tex.width() as f32, mask_tex.height() as f32],
            radius: (GUIDED_RADIUS / s).max(1),
            eps: GUIDED_EPS,
        };
        upload(gpu, &sh.cb_guided, &gc);
        let cb = Some(&sh.cb_guided);
        let (cw, ch) = (cam_tex.width(), cam_tex.height());
        let luma_c = PassConsts {
            rect: [0.0, 0.0, g.0 as f32, g.1 as f32],
            src_size: [cw as f32, ch as f32],
            cam_linear: u32::from(use_sampler(cw, ch, g.0, g.1)),
            ..Default::default()
        };
        let low_c = PassConsts {
            rect: [0.0, 0.0, low.0 as f32, low.1 as f32],
            ..Default::default()
        };
        let full_c = PassConsts {
            rect: [0.0, 0.0, g.0 as f32, g.1 as f32],
            ..Default::default()
        };
        let srv = |t: &Texture| t.srv().cloned();
        pass(
            gpu,
            sh,
            &sh.luma,
            luma,
            g,
            &luma_c,
            cb,
            [srv(cam_tex), None, None],
        )?;
        let m = srv(mask_tex);
        pass(
            gpu,
            sh,
            &sh.stats,
            stats,
            low,
            &low_c,
            cb,
            [srv(luma), m, None],
        )?;
        pass(
            gpu,
            sh,
            &sh.box_h,
            tmp,
            low,
            &low_c,
            cb,
            [srv(stats), None, None],
        )?;
        pass(
            gpu,
            sh,
            &sh.box_v_ab,
            ab,
            low,
            &low_c,
            cb,
            [srv(tmp), None, None],
        )?;
        pass(
            gpu,
            sh,
            &sh.box_h,
            tmp,
            low,
            &low_c,
            cb,
            [srv(ab), None, None],
        )?;
        // Mean a / mean b into `stats` (no longer needed).
        pass(
            gpu,
            sh,
            &sh.box_v,
            stats,
            low,
            &low_c,
            cb,
            [srv(tmp), None, None],
        )?;
        let io = [srv(luma), None, srv(stats)];
        pass(gpu, sh, &sh.guided_out, out, g, &full_c, cb, io)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backdrop_shaders_compile() {
        let b = compile_blobs().unwrap();
        assert_eq!(b.code.len(), ENTRIES.len());
        assert!(b.code.iter().all(|c| !c.is_empty()));
    }

    #[test]
    fn constant_buffers_are_whole_registers() {
        assert_eq!(std::mem::size_of::<BlurConsts>(), 176);
        assert_eq!(std::mem::size_of::<GuidedConsts>(), 32);
    }

    #[test]
    fn blur_kernels_fit_the_constants() {
        for w in [1u32, 4, 100, 640, 1920, 3840, 7680] {
            let (lw, _) = blur_low_size(w, w / 2 + 1);
            assert!(blur_kernel(lw).len() <= MAX_TAPS, "w={w}");
        }
    }
}
