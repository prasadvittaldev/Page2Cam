//! Windows Graphics Capture of one monitor, read back to BGRA frames.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use p2c_core::Frame;
use windows::core::{Interface, BOOL};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HMODULE, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, DEVMODEW, ENUM_CURRENT_SETTINGS,
    HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use crate::{copy_rows, id_from_device, ScreenInfo, Sink};

fn monitors() -> Vec<(HMONITOR, ScreenInfo)> {
    unsafe extern "system" fn add(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the Vec passed below, alive for the whole call.
        let out = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        out.push(m);
        true.into()
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only pushes into `handles`.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(add),
            LPARAM(&mut handles as *mut _ as isize),
        );
    }
    handles
        .into_iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            // SAFETY: cbSize says the buffer is a MONITORINFOEXW.
            let ok =
                unsafe { GetMonitorInfoW(m, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) };
            if !ok.as_bool() {
                return None;
            }
            let len = info
                .szDevice
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(info.szDevice.len());
            let device = String::from_utf16_lossy(&info.szDevice[..len]);
            let r = info.monitorInfo.rcMonitor;
            // Physical pixels (rcMonitor is DPI-scaled for a DPI-unaware process).
            let mut mode = DEVMODEW {
                dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            // SAFETY: szDevice is NUL-terminated; dmSize is set.
            let have_mode = unsafe {
                EnumDisplaySettingsW(
                    windows::core::PCWSTR(info.szDevice.as_ptr()),
                    ENUM_CURRENT_SETTINGS,
                    &mut mode,
                )
            }
            .as_bool();
            let (width, height) = if have_mode {
                (mode.dmPelsWidth, mode.dmPelsHeight)
            } else {
                (
                    (r.right - r.left).max(0) as u32,
                    (r.bottom - r.top).max(0) as u32,
                )
            };
            let index = i as u32 + 1;
            Some((
                m,
                ScreenInfo {
                    id: id_from_device(&device),
                    index,
                    name: format!("Screen {index}"),
                    width,
                    height,
                    // MONITORINFOF_PRIMARY
                    primary: info.monitorInfo.dwFlags & 1 != 0,
                },
            ))
        })
        .collect()
}

pub fn list() -> Result<Vec<ScreenInfo>> {
    Ok(monitors().into_iter().map(|(_, s)| s).collect())
}

/// Start the capture thread; returns once capture runs (or failed to start).
pub fn spawn(
    id: String,
    interval: Duration,
    sink: Sink,
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
) -> Result<JoinHandle<()>> {
    let (tx, rx) = mpsc::channel::<Result<()>>();
    let thread = std::thread::Builder::new()
        .name("p2c-screen".into())
        .spawn(move || {
            // SAFETY: per-thread WinRT init; an already-initialised thread is fine.
            let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
            let mut cap = match Session::open(&id) {
                Ok(c) => {
                    let _ = tx.send(Ok(()));
                    c
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };
            if let Err(e) = cap.run(interval, sink, &stop) {
                tracing::warn!(error = %format!("{e:#}"), "screen capture stopped");
                *error.lock().unwrap_or_else(|p| p.into_inner()) = Some(format!("{e:#}"));
            }
            let _ = cap.session.Close();
            let _ = cap.pool.Close();
        })?;
    match rx.recv() {
        Ok(Ok(())) => Ok(thread),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            Err(anyhow!("screen capture thread ended"))
        }
    }
}

struct Session {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    d3d: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    size: SizeInt32,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
}

const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;

impl Session {
    fn open(id: &str) -> Result<Session> {
        anyhow::ensure!(
            GraphicsCaptureSession::IsSupported().unwrap_or(false),
            "Windows Graphics Capture is not supported here"
        );
        let hmon = monitors()
            .into_iter()
            .find(|(_, s)| s.id.eq_ignore_ascii_case(id))
            .map(|(m, _)| m)
            .ok_or_else(|| anyhow!("no screen {id:?}"))?;
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .context("capture item factory")?;
        // SAFETY: a monitor handle from EnumDisplayMonitors just now.
        let item: GraphicsCaptureItem =
            unsafe { interop.CreateForMonitor(hmon) }.context("CreateForMonitor")?;
        let (mut device, mut ctx) = (None, None);
        // SAFETY: plain device creation on the default hardware adapter.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )
        }
        .context("D3D11CreateDevice for screen capture")?;
        let device: ID3D11Device = device.context("no D3D11 device")?;
        let ctx = ctx.context("no D3D11 context")?;
        let dxgi: IDXGIDevice = device.cast()?;
        // SAFETY: wraps our DXGI device for WinRT.
        let d3d: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }?.cast()?;
        let size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&d3d, FORMAT, 2, size)
            .context("capture frame pool")?;
        let session = pool
            .CreateCaptureSession(&item)
            .context("capture session")?;
        let _ = session.SetIsCursorCaptureEnabled(true);
        session.StartCapture().context("StartCapture")?;
        Ok(Session {
            device,
            ctx,
            d3d,
            pool,
            session,
            size,
            staging: None,
        })
    }

    /// The newest frame waiting in the pool, if any (older ones are dropped).
    fn newest(&self) -> Option<Direct3D11CaptureFrame> {
        let mut last = None;
        while let Ok(f) = self.pool.TryGetNextFrame() {
            last = Some(f);
        }
        last
    }

    fn run(&mut self, interval: Duration, mut sink: Sink, stop: &AtomicBool) -> Result<()> {
        let mut next = Instant::now();
        while !stop.load(Ordering::SeqCst) {
            let now = Instant::now();
            if now < next {
                std::thread::sleep((next - now).min(Duration::from_millis(50)));
                continue;
            }
            next = now + interval;
            let Some(frame) = self.newest() else {
                continue;
            };
            let content = frame.ContentSize()?;
            if content.Width != self.size.Width || content.Height != self.size.Height {
                // Resolution changed: new buffers, this frame is dropped.
                self.size = content;
                self.pool.Recreate(&self.d3d, FORMAT, 2, content)?;
                continue;
            }
            if let Some(f) = self.read(&frame, content)? {
                sink(f);
            }
        }
        Ok(())
    }

    fn read(&mut self, frame: &Direct3D11CaptureFrame, size: SizeInt32) -> Result<Option<Frame>> {
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        // SAFETY: the surface is a D3D11 texture on our device.
        let tex: ID3D11Texture2D = unsafe { access.GetInterface() }?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: plain query.
        unsafe { tex.GetDesc(&mut desc) };
        let (w, h) = (
            (size.Width.max(0) as u32).min(desc.Width),
            (size.Height.max(0) as u32).min(desc.Height),
        );
        if w == 0 || h == 0 {
            return Ok(None);
        }
        let staging = match &self.staging {
            Some((t, sw, sh)) if (*sw, *sh) == (desc.Width, desc.Height) => t.clone(),
            _ => {
                let sd = D3D11_TEXTURE2D_DESC {
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                    ..desc
                };
                let mut t = None;
                // SAFETY: a valid staging description.
                unsafe { self.device.CreateTexture2D(&sd, None, Some(&mut t)) }
                    .context("staging texture")?;
                let t = t.context("no staging texture")?;
                self.staging = Some((t.clone(), desc.Width, desc.Height));
                t
            }
        };
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: same size/format textures; the mapping holds Height rows of
        // RowPitch bytes until Unmap.
        let data = unsafe {
            self.ctx.CopyResource(&staging, &tex);
            self.ctx
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m))
                .context("Map")?;
            let len = m.RowPitch as usize * desc.Height as usize;
            let src = std::slice::from_raw_parts(m.pData as *const u8, len);
            let out = copy_rows(src, m.RowPitch as usize, w, h);
            self.ctx.Unmap(&staging, 0);
            out
        };
        Ok(data.and_then(|d| Frame::from_bgra(w, h, d).ok()))
    }
}
