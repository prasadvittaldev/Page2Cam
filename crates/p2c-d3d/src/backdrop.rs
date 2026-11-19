//! Backdrops and refined edges on the GPU (mirrors `p2c_compositor`'s
//! `BlurCache`, box image and `GuidedScratch`): the blur passes
//! (blur.hlsl), the guided-filter passes (guided.hlsl) and their cached
//! textures. What to draw is decided by `p2c_compositor::plan_compose`, the
//! same function the CPU compositor uses; [`crate::compose`] draws it.
//!
//! The shaders are compiled the first time a backdrop or refined edges are
//! used, so Page + Room with LUT edges costs nothing extra.

use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use p2c_compositor::{
    blur_kernel, blur_low_size, cover_crop, refined_size, GUIDED_EPS, GUIDED_RADIUS,
    GUIDED_SUBSAMPLE,
};
use p2c_core::Frame;
use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Buffer, ID3D11Device, ID3D11PixelShader, ID3D11ShaderResourceView,
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BUFFER_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R32G32B32A32_FLOAT, DXGI_FORMAT_R8_UNORM,
};

use crate::compose::{use_sampler, viewport, PassConsts, COMPOSE_HLSL};
use crate::device::{Gpu, Texture};
use crate::shaders::ps;

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

/// The source of a `Blur`/`GuidedConsts` constant buffer update.
fn bytes<T>(v: &T) -> *const std::ffi::c_void {
    v as *const T as _
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

/// Compiled backdrop / refined-edge shaders and their constant buffers.
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
    pub(crate) fn new(device: &ID3D11Device) -> Result<BackdropShaders> {
        let blur = format!("{COMPOSE_HLSL}\n{BLUR_HLSL}");
        let guided = format!("{COMPOSE_HLSL}\n{GUIDED_HLSL}");
        let b = |e: &str| ps(device, &blur, "blur.hlsl", e);
        let g = |e: &str| ps(device, &guided, "guided.hlsl", e);
        Ok(BackdropShaders {
            cover: b("ps_cover")?,
            blur_h: b("ps_blur_h")?,
            blur_v: b("ps_blur_v")?,
            fill: b("ps_fill")?,
            luma: g("ps_guide_luma")?,
            stats: g("ps_guide_stats")?,
            box_h: g("ps_box_h")?,
            box_v_ab: g("ps_box_v_ab")?,
            box_v: g("ps_box_v")?,
            guided_out: g("ps_guided_out")?,
            cb_pass: cbuffer(device, std::mem::size_of::<PassConsts>())?,
            cb_blur: cbuffer(device, std::mem::size_of::<BlurConsts>())?,
            cb_guided: cbuffer(device, std::mem::size_of::<GuidedConsts>())?,
        })
    }
}

/// One fullscreen-triangle pass into `target` (whole texture), no blending.
/// Takes the context lock itself: never call it while holding it.
fn pass(
    gpu: &Gpu,
    sh: &BackdropShaders,
    shader: &ID3D11PixelShader,
    target: &Texture,
    c0: &PassConsts,
    c1: Option<(&ID3D11Buffer, *const std::ffi::c_void)>,
    srvs: [Option<ID3D11ShaderResourceView>; 3],
) -> Result<()> {
    let rtv = target.rtv().cloned().context("pass target has no RTV")?;
    let (w, h) = (target.width() as f32, target.height() as f32);
    let ctx = gpu.context();
    let s = gpu.shaders();
    let _lock = gpu.lock();
    // SAFETY: resources of this device; state sequence under the lock.
    unsafe {
        ctx.UpdateSubresource(&sh.cb_pass, 0, None, bytes(c0), 0, 0);
        if let Some((cb, data)) = c1 {
            ctx.UpdateSubresource(cb, 0, None, data, 0, 0);
        }
        ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ctx.IASetInputLayout(None);
        ctx.VSSetShader(&s.vs_fullscreen, None);
        ctx.PSSetShader(shader, None);
        ctx.OMSetBlendState(None, None, 0xffff_ffff);
        ctx.RSSetState(None);
        ctx.OMSetRenderTargets(Some(&[Some(rtv)]), None);
        ctx.PSSetConstantBuffers(
            0,
            Some(&[Some(sh.cb_pass.clone()), c1.map(|(cb, _)| cb.clone())]),
        );
        ctx.PSSetSamplers(0, Some(&[Some(s.linear_clamp.clone())]));
        ctx.PSSetShaderResources(0, Some(&srvs));
        ctx.RSSetViewports(Some(&[viewport(0.0, 0.0, w, h)]));
        ctx.Draw(3, 0);
        ctx.OMSetRenderTargets(None, None);
        ctx.PSSetShaderResources(0, Some(&[None, None, None]));
    }
    Ok(())
}

fn rt(
    gpu: &Gpu,
    w: u32,
    h: u32,
    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
) -> Result<Texture> {
    gpu.new_texture(
        w,
        h,
        format,
        D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
    )
}

/// Scale `src` (its crop `crop`, in source pixels) into `dst` with the
/// exact filter, opaque.
fn cover_pass(
    gpu: &Gpu,
    sh: &BackdropShaders,
    src: &Texture,
    crop: (f64, f64, f64, f64),
    dst: &Texture,
) -> Result<()> {
    let c0 = PassConsts {
        rect: [0.0, 0.0, dst.width() as f32, dst.height() as f32],
        src_size: [src.width() as f32, src.height() as f32],
        ..Default::default()
    };
    let c1 = BlurConsts {
        crop: [crop.0 as f32, crop.1 as f32, crop.2 as f32, crop.3 as f32],
        ..Default::default()
    };
    pass(
        gpu,
        sh,
        &sh.cover,
        dst,
        &c0,
        Some((&sh.cb_blur, bytes(&c1))),
        [src.srv().cloned(), None, None],
    )
}

/// A blurred webcam at reduced size (`BlurCache` on the GPU): drawn
/// upscaled with the bilinear sampler. Rebuilt only for another webcam
/// frame (Arc identity), another drawn size or another fit.
pub(crate) struct GpuBlur {
    key: Option<(Arc<Frame>, (u32, u32), bool)>,
    small: Option<Texture>,
    tmp: Option<Texture>,
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
        let (lw, lh) = blur_low_size(w, h);
        let sized = |t: &Option<Texture>| {
            t.as_ref()
                .is_some_and(|t| (t.width(), t.height()) == (lw, lh))
        };
        if !sized(&self.small) {
            self.small = Some(rt(gpu, lw, lh, DXGI_FORMAT_B8G8R8A8_UNORM)?);
            self.tmp = Some(rt(gpu, lw, lh, DXGI_FORMAT_R32G32B32A32_FLOAT)?);
            self.out = Some(rt(gpu, lw, lh, DXGI_FORMAT_B8G8R8A8_UNORM)?);
        }
        let (small, tmp, out) = (
            self.small.as_ref().context("blur small")?,
            self.tmp.as_ref().context("blur tmp")?,
            self.out.as_ref().context("blur out")?,
        );
        let crop = if cover {
            cover_crop(cam.width, cam.height, lw, lh)
        } else {
            (0.0, 0.0, cam.width as f64, cam.height as f64)
        };
        cover_pass(gpu, sh, cam_tex, crop, small)?;
        let kernel = blur_kernel(lw);
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
        let c0 = PassConsts {
            rect: [0.0, 0.0, lw as f32, lh as f32],
            src_size: [lw as f32, lh as f32],
            ..Default::default()
        };
        let cb = Some((&sh.cb_blur, bytes(&c1)));
        pass(
            gpu,
            sh,
            &sh.blur_h,
            tmp,
            &c0,
            cb,
            [small.srv().cloned(), None, None],
        )?;
        pass(
            gpu,
            sh,
            &sh.blur_v,
            out,
            &c0,
            cb,
            [tmp.srv().cloned(), None, None],
        )
    }
}

/// The box image: uploaded once per image (Arc identity), cover-scaled to
/// the box once per image and size.
pub(crate) struct BoxImage {
    src: Arc<Frame>,
    /// `None` for an image that can't be shown (empty, malformed, too big).
    tex: Option<Texture>,
    scaled: Option<((u32, u32), Texture)>,
    pub(crate) uploads: u64,
}

/// Largest texture side D3D11 guarantees (feature level 11).
const MAX_SIDE: u32 = 16384;

impl BoxImage {
    /// The image cover-scaled to `w` x `h` (opaque), or `None` (draw black)
    /// for an image that can't be shown.
    pub(crate) fn scaled<'a>(
        slot: &'a mut Option<BoxImage>,
        gpu: &Gpu,
        sh: &BackdropShaders,
        img: &Arc<Frame>,
        (w, h): (u32, u32),
    ) -> Result<Option<&'a Texture>> {
        if !slot.as_ref().is_some_and(|b| Arc::ptr_eq(&b.src, img)) {
            let uploads = slot.as_ref().map_or(0, |b| b.uploads);
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
                scaled: None,
                uploads: uploads + 1,
            });
        }
        let b = slot.as_mut().context("box image")?;
        let Some(tex) = &b.tex else {
            return Ok(None);
        };
        if !b.scaled.as_ref().is_some_and(|(size, _)| *size == (w, h)) {
            let dst = match b.scaled.take() {
                Some((_, t)) if (t.width(), t.height()) == (w, h) => t,
                _ => rt(gpu, w, h, DXGI_FORMAT_B8G8R8A8_UNORM)?,
            };
            cover_pass(gpu, sh, tex, cover_crop(img.width, img.height, w, h), &dst)?;
            b.scaled = Some(((w, h), dst));
        }
        Ok(b.scaled.as_ref().map(|(_, t)| t))
    }
}

/// Textures of the guided filter at one refined size.
pub(crate) struct Guided {
    size: (u32, u32),
    luma: Texture,
    stats: Texture,
    tmp: Texture,
    ab: Texture,
    out: Texture,
}

impl Guided {
    /// The refined matte (R8) of a `w` x `h` PIP: the model mask `mask_tex`
    /// upsampled with the webcam `cam_tex` as guide, at `refined_size(w, h)`.
    pub(crate) fn run<'a>(
        slot: &'a mut Option<Guided>,
        gpu: &Gpu,
        sh: &BackdropShaders,
        cam_tex: &Texture,
        mask_tex: &Texture,
        (w, h): (u32, u32),
    ) -> Result<&'a Texture> {
        let (gw, gh) = refined_size(w, h);
        let s = GUIDED_SUBSAMPLE as u32;
        let (lw, lh) = (gw.div_ceil(s), gh.div_ceil(s));
        if !slot.as_ref().is_some_and(|g| g.size == (gw, gh)) {
            *slot = Some(Guided {
                size: (gw, gh),
                luma: rt(gpu, gw, gh, DXGI_FORMAT_R8_UNORM)?,
                stats: rt(gpu, lw, lh, DXGI_FORMAT_R32G32B32A32_FLOAT)?,
                tmp: rt(gpu, lw, lh, DXGI_FORMAT_R32G32B32A32_FLOAT)?,
                ab: rt(gpu, lw, lh, DXGI_FORMAT_R32G32B32A32_FLOAT)?,
                out: rt(gpu, gw, gh, DXGI_FORMAT_R8_UNORM)?,
            });
        }
        let g = slot.as_ref().context("guided targets")?;
        let gc = GuidedConsts {
            g_size: [gw as f32, gh as f32],
            low_size: [lw as f32, lh as f32],
            mask_size: [mask_tex.width() as f32, mask_tex.height() as f32],
            radius: (GUIDED_RADIUS / s).max(1),
            eps: GUIDED_EPS,
        };
        let cb = Some((&sh.cb_guided, bytes(&gc)));
        let (cw, ch) = (cam_tex.width(), cam_tex.height());
        let luma_c = PassConsts {
            rect: [0.0, 0.0, gw as f32, gh as f32],
            src_size: [cw as f32, ch as f32],
            cam_linear: u32::from(use_sampler(cw, ch, gw, gh)),
            ..Default::default()
        };
        let low = PassConsts {
            rect: [0.0, 0.0, lw as f32, lh as f32],
            ..Default::default()
        };
        let full = PassConsts {
            rect: [0.0, 0.0, gw as f32, gh as f32],
            ..Default::default()
        };
        let srv = |t: &Texture| t.srv().cloned();
        pass(
            gpu,
            sh,
            &sh.luma,
            &g.luma,
            &luma_c,
            cb,
            [srv(cam_tex), None, None],
        )?;
        pass(
            gpu,
            sh,
            &sh.stats,
            &g.stats,
            &low,
            cb,
            [srv(&g.luma), srv(mask_tex), None],
        )?;
        pass(
            gpu,
            sh,
            &sh.box_h,
            &g.tmp,
            &low,
            cb,
            [srv(&g.stats), None, None],
        )?;
        pass(
            gpu,
            sh,
            &sh.box_v_ab,
            &g.ab,
            &low,
            cb,
            [srv(&g.tmp), None, None],
        )?;
        pass(
            gpu,
            sh,
            &sh.box_h,
            &g.tmp,
            &low,
            cb,
            [srv(&g.ab), None, None],
        )?;
        // Mean a / mean b into `stats` (no longer needed).
        pass(
            gpu,
            sh,
            &sh.box_v,
            &g.stats,
            &low,
            cb,
            [srv(&g.tmp), None, None],
        )?;
        pass(
            gpu,
            sh,
            &sh.guided_out,
            &g.out,
            &full,
            cb,
            [srv(&g.luma), None, srv(&g.stats)],
        )?;
        Ok(&g.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::compile;

    #[test]
    fn backdrop_shaders_compile() {
        let blur = format!("{COMPOSE_HLSL}\n{BLUR_HLSL}");
        for e in ["ps_cover", "ps_blur_h", "ps_blur_v", "ps_fill"] {
            compile(&blur, "blur.hlsl", e, "ps_5_0").unwrap();
        }
        let guided = format!("{COMPOSE_HLSL}\n{GUIDED_HLSL}");
        for e in [
            "ps_guide_luma",
            "ps_guide_stats",
            "ps_box_h",
            "ps_box_v_ab",
            "ps_box_v",
            "ps_guided_out",
        ] {
            compile(&guided, "guided.hlsl", e, "ps_5_0").unwrap();
        }
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
