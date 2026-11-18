//! Runtime-compiled shaders (`D3DCompile` from d3dcompiler_47.dll, so no
//! build-time tooling) and the BGRA -> NV12 pass.

use anyhow::{anyhow, bail, Context, Result};
use windows::core::PCSTR;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Buffer, ID3D11Device, ID3D11PixelShader, ID3D11SamplerState, ID3D11VertexShader,
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BUFFER_DESC, D3D11_COMPARISON_NEVER, D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_SAMPLER_DESC,
    D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM};

use crate::device::{check_nv12_pair, Gpu, Texture};

const NV12_HLSL: &str = include_str!("hlsl/nv12.hlsl");

/// Compile one entry point; the error carries the compiler's message.
pub(crate) fn compile(src: &str, name: &str, entry: &str, target: &str) -> Result<Vec<u8>> {
    let cname = format!("{name}\0");
    let centry = format!("{entry}\0");
    let ctarget = format!("{target}\0");
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: all strings are NUL-terminated and outlive the call.
    let r = unsafe {
        D3DCompile(
            src.as_ptr() as _,
            src.len(),
            PCSTR(cname.as_ptr()),
            None,
            None,
            PCSTR(centry.as_ptr()),
            PCSTR(ctarget.as_ptr()),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    let blob_bytes = |b: &ID3DBlob| {
        // SAFETY: the blob owns GetBufferSize bytes at GetBufferPointer.
        unsafe {
            std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())
                .to_vec()
        }
    };
    if let Err(e) = r {
        let msg = errors
            .as_ref()
            .map(|b| {
                String::from_utf8_lossy(&blob_bytes(b))
                    .trim_end_matches('\0')
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();
        return Err(anyhow!(
            "D3DCompile {name}:{entry} ({target}) failed: {e}: {msg}"
        ));
    }
    Ok(blob_bytes(&code.context("D3DCompile returned no code")?))
}

/// Compiled shaders and fixed state, created once per [`Gpu`].
pub(crate) struct Shaders {
    pub(crate) vs_fullscreen: ID3D11VertexShader,
    ps_y: ID3D11PixelShader,
    ps_uv: ID3D11PixelShader,
    linear_clamp: ID3D11SamplerState,
    /// 16-byte constant buffer for the NV12 pass.
    nv12_cb: ID3D11Buffer,
}

fn vs(device: &ID3D11Device, src: &str, name: &str, entry: &str) -> Result<ID3D11VertexShader> {
    let code = compile(src, name, entry, "vs_5_0")?;
    let mut s = None;
    // SAFETY: valid bytecode slice and out-pointer.
    unsafe { device.CreateVertexShader(&code, None, Some(&mut s)) }
        .with_context(|| format!("CreateVertexShader {entry}"))?;
    s.context("CreateVertexShader returned nothing")
}

pub(crate) fn ps(
    device: &ID3D11Device,
    src: &str,
    name: &str,
    entry: &str,
) -> Result<ID3D11PixelShader> {
    let code = compile(src, name, entry, "ps_5_0")?;
    let mut s = None;
    // SAFETY: valid bytecode slice and out-pointer.
    unsafe { device.CreatePixelShader(&code, None, Some(&mut s)) }
        .with_context(|| format!("CreatePixelShader {entry}"))?;
    s.context("CreatePixelShader returned nothing")
}

impl Shaders {
    pub(crate) fn new(device: &ID3D11Device) -> Result<Shaders> {
        let sd = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MipLODBias: 0.0,
            MaxAnisotropy: 1,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            BorderColor: [0.0; 4],
            MinLOD: 0.0,
            MaxLOD: f32::MAX,
        };
        let bd = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        let mut sampler = None;
        let mut cb = None;
        // SAFETY: valid descs and out-pointers.
        unsafe {
            device
                .CreateSamplerState(&sd, Some(&mut sampler))
                .context("CreateSamplerState")?;
            device
                .CreateBuffer(&bd, None, Some(&mut cb))
                .context("CreateBuffer")?;
        }
        Ok(Shaders {
            vs_fullscreen: vs(device, NV12_HLSL, "nv12.hlsl", "vs_fullscreen")?,
            ps_y: ps(device, NV12_HLSL, "nv12.hlsl", "ps_y")?,
            ps_uv: ps(device, NV12_HLSL, "nv12.hlsl", "ps_uv")?,
            linear_clamp: sampler.context("no sampler")?,
            nv12_cb: cb.context("no buffer")?,
        })
    }
}

fn viewport(w: u32, h: u32) -> D3D11_VIEWPORT {
    D3D11_VIEWPORT {
        TopLeftX: 0.0,
        TopLeftY: 0.0,
        Width: w as f32,
        Height: h as f32,
        MinDepth: 0.0,
        MaxDepth: 1.0,
    }
}

/// Convert a BGRA texture (with a shader resource view) to NV12 at
/// `dw` x `dh` (even, non-zero): a Y plane (R8, `dw` x `dh`) and a UV plane
/// (R8G8, half size), BT.709 limited range, matching
/// `p2c_core::bgra_to_nv12`. Scales bilinearly when the size differs.
///
/// The output textures are owned by `gpu` and reused by the next call with
/// the same size: queue their copy (e.g. [`crate::Readback::submit`]) before
/// converting the next frame. (Copies queued earlier are unaffected; the
/// context executes in order.)
pub fn bgra_to_nv12_gpu(gpu: &Gpu, src: &Texture, dw: u32, dh: u32) -> Result<(Texture, Texture)> {
    if dw == 0 || dh == 0 || !dw.is_multiple_of(2) || !dh.is_multiple_of(2) {
        bail!("NV12 size must be even and non-zero, got {dw}x{dh}");
    }
    let (y, uv) = {
        let mut cache = gpu.nv12_targets().lock().unwrap_or_else(|p| p.into_inner());
        match &*cache {
            Some((y, uv)) if (y.width(), y.height()) == (dw, dh) => (y.clone(), uv.clone()),
            _ => {
                let bind = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
                let y = gpu.new_texture(dw, dh, DXGI_FORMAT_R8_UNORM, bind)?;
                let uv = gpu.new_texture(dw / 2, dh / 2, DXGI_FORMAT_R8G8_UNORM, bind)?;
                *cache = Some((y.clone(), uv.clone()));
                (y, uv)
            }
        }
    };
    bgra_to_nv12_into(gpu, src, &y, &uv)?;
    Ok((y, uv))
}

/// Like [`bgra_to_nv12_gpu`], into caller-owned planes: `y` (R8, render
/// target, even non-zero size) and `uv` (R8G8, render target, half size).
/// For callers that keep their own targets (e.g. two sizes per frame).
pub fn bgra_to_nv12_into(gpu: &Gpu, src: &Texture, y: &Texture, uv: &Texture) -> Result<()> {
    check_nv12_pair(y, uv)?;
    let (dw, dh) = (y.width(), y.height());
    if dw == 0 || dh == 0 || !dw.is_multiple_of(2) || !dh.is_multiple_of(2) {
        bail!("NV12 size must be even and non-zero, got {dw}x{dh}");
    }
    let srv = src
        .srv()
        .context("bgra_to_nv12: source has no shader resource view")?
        .clone();
    let (Some(y_rtv), Some(uv_rtv)) = (y.rtv().cloned(), uv.rtv().cloned()) else {
        bail!("bgra_to_nv12: NV12 planes need render target views");
    };
    let s = gpu.shaders();
    let consts: [f32; 4] = [1.0 / dw as f32, 1.0 / dh as f32, 0.0, 0.0];
    let ctx = gpu.context();
    let _lock = gpu.lock();
    // SAFETY: all resources belong to this device; the lock keeps other
    // threads from changing pipeline state in between.
    unsafe {
        ctx.UpdateSubresource(&s.nv12_cb, 0, None, consts.as_ptr() as _, 0, 0);
        ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ctx.IASetInputLayout(None);
        ctx.VSSetShader(&s.vs_fullscreen, None);
        ctx.PSSetShaderResources(0, Some(&[Some(srv)]));
        ctx.PSSetSamplers(0, Some(&[Some(s.linear_clamp.clone())]));
        ctx.PSSetConstantBuffers(0, Some(&[Some(s.nv12_cb.clone())]));
        ctx.OMSetBlendState(None, None, 0xffff_ffff);
        ctx.RSSetState(None);

        ctx.OMSetRenderTargets(Some(&[Some(y_rtv)]), None);
        ctx.RSSetViewports(Some(&[viewport(dw, dh)]));
        ctx.PSSetShader(&s.ps_y, None);
        ctx.Draw(3, 0);

        ctx.OMSetRenderTargets(Some(&[Some(uv_rtv)]), None);
        ctx.RSSetViewports(Some(&[viewport(dw / 2, dh / 2)]));
        ctx.PSSetShader(&s.ps_uv, None);
        ctx.Draw(3, 0);

        // Unbind so the planes can be read and the source written elsewhere.
        ctx.OMSetRenderTargets(None, None);
        ctx.PSSetShaderResources(0, Some(&[None]));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_shader_compiles() {
        for (e, t) in [
            ("vs_fullscreen", "vs_5_0"),
            ("ps_y", "ps_5_0"),
            ("ps_uv", "ps_5_0"),
        ] {
            compile(NV12_HLSL, "nv12.hlsl", e, t).unwrap();
        }
    }

    #[test]
    fn compile_error_carries_message() {
        let e = compile(
            "float4 main() : SV_Target { return oops; }",
            "bad.hlsl",
            "main",
            "ps_5_0",
        )
        .unwrap_err();
        let s = format!("{e:#}");
        assert!(s.contains("oops"), "{s}");
    }
}
