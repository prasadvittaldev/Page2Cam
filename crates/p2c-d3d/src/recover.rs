//! The GPU path as the ticker sees it: a [`GpuProducer`] that recreates its
//! device once after device loss, and the [`PageHolder`] Chromium's shared
//! textures go through (so they reach the new device's [`PageInput`]).

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use p2c_compositor::{Output, OutputProducer, OutputSpec, TickInputs};

use crate::compose::GpuProducer;
use crate::device::{is_device_lost, Gpu};
use crate::page::PageInput;

/// Where the browser's shared page textures go: the current device's
/// [`PageInput`], swapped when the device is recreated. Cloning shares it.
#[derive(Clone, Default)]
pub struct PageHolder(Arc<Mutex<Option<PageInput>>>);

impl PageHolder {
    /// Call from `on_accelerated_paint` (see [`PageInput::on_shared_texture`]).
    /// While the device is being recreated there is no page input, and the
    /// frame is dropped (`Ok`).
    pub fn on_shared_texture(&self, handle: isize) -> Result<()> {
        // Not held during the copy: the ticker may swap the input meanwhile.
        match self.current() {
            Some(p) => p.on_shared_texture(handle),
            None => Ok(()),
        }
    }

    /// The current page input (`None` while the device is being recreated).
    pub fn current(&self) -> Option<PageInput> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn set(&self, page: Option<PageInput>) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = page;
    }
}

/// How long a device rebuild may take before it counts as failed (the
/// ticker repeats the last output meanwhile, so this bounds the freeze).
const REBUILD_DEADLINE: Duration = Duration::from_secs(1);

/// A [`GpuProducer`] on one adapter that survives one device loss: on the
/// first error that [`is_device_lost`], it builds a new `Gpu`, `PageInput`
/// (published through the [`PageHolder`]) and `GpuProducer` on the same
/// adapter on a background thread, returning that error once and then
/// `Ok(None)` (the ticker re-pushes the last output) until the new device is
/// ready (at most 1 s; a rebuild that takes longer is given up and every
/// later call fails). A later device loss, or a failed rebuild, is returned as an error,
/// and the ticker falls back to the CPU.
pub struct RecoveringProducer {
    luid: u64,
    spec: OutputSpec,
    holder: PageHolder,
    producer: GpuProducer,
    /// A running rebuild and when it started.
    rebuild: Option<(JoinHandle<Result<GpuProducer>>, Instant)>,
    /// The rebuild missed its deadline: every call fails from then on.
    gave_up: bool,
    /// Tests: sleep this long in the rebuild thread (a hung driver).
    rebuild_delay: Duration,
    recreated: bool,
}

/// `Gpu`, `PageInput` and `GpuProducer` on one new device (the page input and
/// the producer must share the device).
fn build(luid: u64, spec: OutputSpec, holder: &PageHolder) -> Result<GpuProducer> {
    let gpu = Gpu::new(luid).context("D3D11 device")?;
    let page = PageInput::new(&gpu, spec.width, spec.height).context("page texture")?;
    let producer = GpuProducer::new(gpu, page.clone(), spec).context("GPU compositor")?;
    holder.set(Some(page));
    Ok(producer)
}

impl RecoveringProducer {
    /// Create the device on the adapter with this LUID (from a fresh
    /// `enumerate()`), and route `holder` to its page input.
    pub fn new(luid: u64, spec: OutputSpec, holder: PageHolder) -> Result<RecoveringProducer> {
        let producer = build(luid, spec, &holder)?;
        Ok(RecoveringProducer {
            luid,
            spec,
            holder,
            producer,
            rebuild: None,
            rebuild_delay: Duration::ZERO,
            gave_up: false,
            recreated: false,
        })
    }

    /// The current device.
    pub fn gpu(&self) -> &Gpu {
        self.producer.gpu()
    }

    /// Tests: make the device rebuild hang for `d` (like a driver reset).
    #[doc(hidden)]
    pub fn delay_rebuild_for_test(&mut self, d: Duration) {
        self.rebuild_delay = d;
    }

    /// Start rebuilding the device in the background.
    fn start_rebuild(&mut self) -> Result<()> {
        self.holder.set(None);
        let (luid, spec, holder) = (self.luid, self.spec, self.holder.clone());
        let delay = self.rebuild_delay;
        let h = std::thread::Builder::new()
            .name("p2c-gpu-rebuild".into())
            .spawn(move || {
                std::thread::sleep(delay);
                build(luid, spec, &holder)
            })
            .context("spawn GPU rebuild thread")?;
        self.rebuild = Some((h, Instant::now()));
        Ok(())
    }

    /// A finished rebuild: swap the producer in. `Ok(false)`: still running.
    fn finish_rebuild(&mut self) -> Result<bool> {
        if self.gave_up {
            bail!("the GPU device could not be recreated within {REBUILD_DEADLINE:?}");
        }
        if let Some((_, since)) = &self.rebuild {
            if !self.rebuild.as_ref().is_some_and(|(h, _)| h.is_finished())
                && since.elapsed() >= REBUILD_DEADLINE
            {
                // A hung driver (TDR, reset): give up so the ticker falls back
                // to the CPU. Dropping the handle detaches the thread.
                self.rebuild = None;
                self.gave_up = true;
                tracing::warn!(
                    "GPU device rebuild still running after {REBUILD_DEADLINE:?}; giving up"
                );
                bail!("the GPU device could not be recreated within {REBUILD_DEADLINE:?}");
            }
        }
        let Some((h, _)) = self.rebuild.take_if(|(h, _)| h.is_finished()) else {
            return Ok(self.rebuild.is_none());
        };
        let p = h
            .join()
            .map_err(|_| anyhow!("GPU device rebuild panicked"))?
            .context("recreating the GPU device after device loss")?;
        self.producer = p;
        tracing::info!("GPU device recreated");
        Ok(true)
    }
}

impl OutputProducer for RecoveringProducer {
    fn produce(&mut self, inputs: &TickInputs, preview_due: bool) -> Result<Option<Output>> {
        if !self.finish_rebuild()? {
            return Ok(None);
        }
        match self.producer.produce(inputs, preview_due) {
            Err(e) if is_device_lost(&e) && !self.recreated => {
                self.recreated = true;
                tracing::warn!(error = %format!("{e:#}"), "GPU device lost; recreating it once");
                self.start_rebuild()?;
                Err(e.context("GPU device lost (recreating it)"))
            }
            r => r,
        }
    }

    fn name(&self) -> String {
        "GPU".into()
    }
}
