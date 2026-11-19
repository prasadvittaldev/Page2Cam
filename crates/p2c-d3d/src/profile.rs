//! GPU pass timing with D3D11 timestamp queries, for benches and diagnostics.
//! Off by default; reading the results waits for the GPU, so never enable it
//! on the live output path.

use anyhow::{bail, Context, Result};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Query, D3D11_QUERY_DATA_TIMESTAMP_DISJOINT, D3D11_QUERY_DESC, D3D11_QUERY_TIMESTAMP,
    D3D11_QUERY_TIMESTAMP_DISJOINT,
};

use crate::device::Gpu;

/// Timestamps around the passes of one `produce` call.
pub(crate) struct GpuProfiler {
    disjoint: ID3D11Query,
    stamps: Vec<ID3D11Query>,
    names: Vec<&'static str>,
    open: bool,
}

pub(crate) fn query(
    gpu: &Gpu,
    kind: windows::Win32::Graphics::Direct3D11::D3D11_QUERY,
) -> Result<ID3D11Query> {
    let desc = D3D11_QUERY_DESC {
        Query: kind,
        MiscFlags: 0,
    };
    let mut q = None;
    // SAFETY: valid desc and out-pointer.
    unsafe { gpu.device().CreateQuery(&desc, Some(&mut q)) }.context("CreateQuery")?;
    q.context("CreateQuery returned nothing")
}

/// Spin until the query's data is available (S_OK, not S_FALSE).
pub(crate) fn wait_data<T: Default>(gpu: &Gpu, q: &ID3D11Query) -> Result<T> {
    let ctx = gpu.context();
    let mut out = T::default();
    let start = std::time::Instant::now();
    loop {
        // windows-rs maps S_FALSE ("not ready") to Ok, so call through the
        // vtable to see the raw HRESULT.
        // SAFETY: `out` is a writable T of the size passed.
        let hr = unsafe {
            (Interface::vtable(ctx).GetData)(
                ctx.as_raw(),
                q.as_raw(),
                &mut out as *mut T as _,
                std::mem::size_of::<T>() as u32,
                0,
            )
        };
        hr.ok().context("GetData")?;
        if hr.0 == 0 {
            return Ok(out);
        }
        if start.elapsed().as_secs() > 2 {
            bail!("GPU timestamp query not ready after 2 s");
        }
        std::thread::yield_now();
    }
}

/// Lazily created profiler state (queries need the device).
pub(crate) struct Profiler {
    inner: Option<GpuProfiler>,
}

impl Profiler {
    pub(crate) fn new() -> Profiler {
        Profiler { inner: None }
    }

    /// Start a frame: a disjoint query around all marks, and the first mark.
    pub(crate) fn begin(&mut self, gpu: &Gpu) -> Result<()> {
        if self.inner.is_none() {
            self.inner = Some(GpuProfiler {
                disjoint: query(gpu, D3D11_QUERY_TIMESTAMP_DISJOINT)?,
                stamps: Vec::new(),
                names: Vec::new(),
                open: false,
            });
        }
        let p = self.inner.as_mut().expect("just set");
        p.names.clear();
        p.open = true;
        let _lock = gpu.lock();
        // SAFETY: a query of this device.
        unsafe { gpu.context().Begin(&p.disjoint) };
        drop(_lock);
        self.mark(gpu, "start")
    }

    /// A timestamp after the work queued so far, named for that work.
    pub(crate) fn mark(&mut self, gpu: &Gpu, name: &'static str) -> Result<()> {
        let Some(p) = self.inner.as_mut().filter(|p| p.open) else {
            return Ok(());
        };
        let i = p.names.len();
        if p.stamps.len() <= i {
            p.stamps.push(query(gpu, D3D11_QUERY_TIMESTAMP)?);
        }
        let _lock = gpu.lock();
        // SAFETY: a query of this device.
        unsafe { gpu.context().End(&p.stamps[i]) };
        p.names.push(name);
        Ok(())
    }

    /// End the frame and wait for its timings: (name, GPU ms since the
    /// previous mark) for each mark after "start". Merges marks of the same
    /// name. `None` if the GPU clock was disjoint (e.g. a power state change).
    pub(crate) fn finish(&mut self, gpu: &Gpu) -> Result<Option<Vec<(&'static str, f64)>>> {
        let Some(p) = self.inner.as_mut().filter(|p| p.open) else {
            return Ok(None);
        };
        p.open = false;
        {
            let _lock = gpu.lock();
            // SAFETY: a query of this device.
            unsafe {
                gpu.context().End(&p.disjoint);
                gpu.context().Flush();
            }
        }
        let dj: D3D11_QUERY_DATA_TIMESTAMP_DISJOINT = wait_data(gpu, &p.disjoint)?;
        let mut ts = Vec::with_capacity(p.names.len());
        for q in &p.stamps[..p.names.len()] {
            ts.push(wait_data::<u64>(gpu, q)?);
        }
        if dj.Disjoint.as_bool() || dj.Frequency == 0 {
            return Ok(None);
        }
        let mut out: Vec<(&'static str, f64)> = Vec::new();
        for i in 1..ts.len() {
            let ms = ts[i].saturating_sub(ts[i - 1]) as f64 * 1e3 / dj.Frequency as f64;
            match out.iter_mut().find(|(n, _)| *n == p.names[i]) {
                Some((_, v)) => *v += ms,
                None => out.push((p.names[i], ms)),
            }
        }
        Ok(Some(out))
    }
}
