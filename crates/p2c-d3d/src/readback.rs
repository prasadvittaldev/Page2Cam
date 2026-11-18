//! Asynchronous NV12 readback: a ring of 3 staging pairs, mapping the frame
//! submitted two submits ago so the CPU never waits on fresh GPU work.

use anyhow::Result;
use p2c_core::Nv12Frame;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM};

use crate::device::{check_nv12_pair, Gpu, Texture};

const DEPTH: usize = 3;

struct Slot {
    y: ID3D11Texture2D,
    uv: ID3D11Texture2D,
    w: u32,
    h: u32,
    pending: bool,
}

/// Ring of 3 NV12 staging pairs. After submits 1..=N, [`Readback::take`]
/// returns frame N−2 (once), `None` until the ring has filled.
#[derive(Default)]
pub struct Readback {
    gpu: Option<Gpu>,
    slots: [Option<Slot>; DEPTH],
    next: usize,
}

// SAFETY: staging textures are free-threaded D3D11 resources; context use goes
// through `Gpu` (multithread protected).
unsafe impl Send for Readback {}

impl Readback {
    pub fn new() -> Readback {
        Readback::default()
    }

    /// Queue a copy of the Y (R8) and UV (R8G8, half size) planes. A
    /// different `gpu` (another device, even on the same adapter) resets the
    /// ring; a different size rebuilds the slot.
    pub fn submit(&mut self, gpu: &Gpu, y: &Texture, uv: &Texture) -> Result<()> {
        check_nv12_pair(y, uv)?;
        if !self.gpu.as_ref().is_some_and(|g| g.same(gpu)) {
            *self = Readback::default();
            self.gpu = Some(gpu.clone());
        }
        let (w, h) = (y.width(), y.height());
        let i = self.next;
        if self.slots[i].as_ref().is_none_or(|s| (s.w, s.h) != (w, h)) {
            self.slots[i] = Some(Slot {
                y: gpu.staging_like(w, h, DXGI_FORMAT_R8_UNORM)?,
                uv: gpu.staging_like(w / 2, h / 2, DXGI_FORMAT_R8G8_UNORM)?,
                w,
                h,
                pending: false,
            });
        }
        let s = self.slots[i].as_mut().expect("slot just filled");
        let ctx = gpu.context();
        {
            let _lock = gpu.lock();
            // SAFETY: same size/format resources of this device.
            unsafe {
                ctx.CopyResource(&s.y, y.raw());
                ctx.CopyResource(&s.uv, uv.raw());
            }
        }
        s.pending = true;
        self.next = (i + 1) % DEPTH;
        Ok(())
    }

    /// Map the oldest queued frame (submitted two submits ago) into an
    /// [`Nv12Frame`]. `None` until 3 frames were submitted, or if it was
    /// already taken. Normally the GPU finished it long ago; if not, the map
    /// waits for it.
    pub fn take(&mut self) -> Result<Option<Nv12Frame>> {
        let Some(gpu) = self.gpu.clone() else {
            return Ok(None);
        };
        let Some(s) = self.slots[self.next].as_mut().filter(|s| s.pending) else {
            return Ok(None);
        };
        s.pending = false;
        let mut data = Vec::with_capacity(s.w as usize * s.h as usize * 3 / 2);
        gpu.read_staging(&s.y, &mut data)?;
        gpu.read_staging(&s.uv, &mut data)?;
        Ok(Some(Nv12Frame::new(s.w, s.h, data)?))
    }
}
