//! The GPU path as the ticker sees it: a [`GpuProducer`] that recreates its
//! device once after device loss, moves once to the adapter Chromium really
//! renders on, and the [`PageHolder`] Chromium's shared textures go through
//! (so they reach the new device's [`PageInput`]).

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use p2c_compositor::{Output, OutputProducer, OutputSpec, TickInputs};

use crate::adapters::AdapterInfo;
use crate::compose::GpuProducer;
use crate::device::{is_device_lost, Gpu};
use crate::page::PageInput;

/// Where the browser's shared page textures go: the current device's
/// [`PageInput`], swapped when the device is recreated. Cloning shares it.
#[derive(Clone, Default)]
pub struct PageHolder(Arc<Mutex<HolderState>>);

#[derive(Default)]
struct HolderState {
    page: Option<PageInput>,
    /// Bumped by every rebuild start and give-up: a rebuild thread publishes
    /// its page input only while its generation is current (a given-up
    /// rebuild that finishes late must not take Chromium's frames).
    generation: u64,
}

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
        self.lock().page.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HolderState> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Drop the page input (frames are dropped until the next publish) and
    /// start a new generation; returns it.
    fn begin(&self) -> u64 {
        let mut s = self.lock();
        s.page = None;
        s.generation += 1;
        s.generation
    }

    /// Route frames to `page` if `generation` is still current.
    fn publish(&self, generation: u64, page: PageInput) -> bool {
        let mut s = self.lock();
        let current = s.generation == generation;
        if current {
            s.page = Some(page);
        }
        current
    }
}

/// How long a rebuild after device loss may take before it counts as failed
/// (the ticker repeats the last output meanwhile, so this bounds the freeze;
/// the ticker's wait for the CPU page counts from the same start).
const REBUILD_DEADLINE: Duration = Duration::from_secs(1);

/// How long moving to Chromium's adapter may take (a sleeping discrete GPU
/// can need 1-3 s to wake up). The old device keeps producing meanwhile, so
/// nothing freezes.
const RETARGET_DEADLINE: Duration = Duration::from_secs(5);

/// Why a device is being built.
enum Purpose {
    /// The device was lost: same adapter, output held meanwhile.
    Recover,
    /// Chromium's textures live on this other adapter: the old device keeps
    /// producing meanwhile.
    Retarget(AdapterInfo),
}

struct Rebuild {
    handle: JoinHandle<Result<GpuProducer>>,
    since: Instant,
    purpose: Purpose,
}

impl Rebuild {
    fn deadline(&self) -> Duration {
        match self.purpose {
            Purpose::Recover => REBUILD_DEADLINE,
            Purpose::Retarget(_) => RETARGET_DEADLINE,
        }
    }
}

/// Called with the new adapter after a move to Chromium's adapter.
pub type AdapterChange = Box<dyn FnMut(&AdapterInfo) + Send>;

/// A [`GpuProducer`] that survives one device loss and one adapter mismatch.
///
/// Device loss: on the first error that [`is_device_lost`], it builds a new
/// `Gpu`, `PageInput` (published through the [`PageHolder`]) and
/// `GpuProducer` on the same adapter on a background thread, returning that
/// error once and then `Ok(None)` (the ticker re-pushes the last output)
/// until the new device is ready (at most 1 s).
///
/// Adapter mismatch (Chromium ignored `use-adapter-luid` and renders on
/// another GPU): once the page input reports
/// [`PageInput::adapter_mismatch`], it builds everything on that adapter
/// (found in a fresh enumeration) while the old device keeps producing,
/// switches over, and calls the [`RecoveringProducer::on_adapter_change`]
/// hook. Done at most once.
///
/// A rebuild that fails or misses its deadline, or a second device loss,
/// makes every later call fail, and the ticker falls back to the CPU.
pub struct RecoveringProducer {
    luid: u64,
    spec: OutputSpec,
    holder: PageHolder,
    producer: GpuProducer,
    rebuild: Option<Rebuild>,
    /// Set once a rebuild failed or timed out: every call fails with it.
    dead: Option<String>,
    /// When the output started being held (device-loss rebuild), until it
    /// produces again.
    held_since: Option<Instant>,
    /// Tests: sleep this long in the rebuild thread (a hung driver).
    rebuild_delay: Duration,
    recreated: bool,
    retargeted: bool,
    on_adapter_change: Option<AdapterChange>,
}

/// `Gpu`, `PageInput` and `GpuProducer` on one new device (the page input and
/// the producer must share the device); the page input is published if
/// `generation` is still the holder's.
fn build(
    luid: u64,
    spec: OutputSpec,
    holder: &PageHolder,
    generation: u64,
) -> Result<GpuProducer> {
    let gpu = Gpu::new(luid).context("D3D11 device")?;
    let page = PageInput::new(&gpu, spec.width, spec.height).context("page texture")?;
    let producer = GpuProducer::new(gpu, page.clone(), spec).context("GPU compositor")?;
    anyhow::ensure!(
        holder.publish(generation, page),
        "GPU device rebuild was given up"
    );
    Ok(producer)
}

impl RecoveringProducer {
    /// Create the device on the adapter with this LUID (from a fresh
    /// `enumerate()`), and route `holder` to its page input.
    pub fn new(luid: u64, spec: OutputSpec, holder: PageHolder) -> Result<RecoveringProducer> {
        let generation = holder.begin();
        let producer = build(luid, spec, &holder, generation)?;
        Ok(RecoveringProducer {
            luid,
            spec,
            holder,
            producer,
            rebuild: None,
            dead: None,
            held_since: None,
            rebuild_delay: Duration::ZERO,
            recreated: false,
            retargeted: false,
            on_adapter_change: None,
        })
    }

    /// Called (on the ticker thread) after the producer moved to the
    /// adapter Chromium renders on.
    pub fn on_adapter_change(&mut self, f: AdapterChange) {
        self.on_adapter_change = Some(f);
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

    /// Fail every call from now on with `msg` (the ticker falls back).
    fn die(&mut self, msg: String) -> anyhow::Error {
        self.holder.begin(); // no page input; a late rebuild can't publish
        self.dead = Some(msg.clone());
        anyhow!(msg)
    }

    /// Start building a device in the background.
    fn start_rebuild(&mut self, purpose: Purpose) -> Result<()> {
        let luid = match &purpose {
            Purpose::Recover => self.luid,
            Purpose::Retarget(a) => a.luid,
        };
        let generation = self.holder.begin();
        let (spec, holder, delay) = (self.spec, self.holder.clone(), self.rebuild_delay);
        let handle = std::thread::Builder::new()
            .name("p2c-gpu-rebuild".into())
            .spawn(move || {
                std::thread::sleep(delay);
                build(luid, spec, &holder, generation)
            })
            .context("spawn GPU rebuild thread")?;
        if matches!(purpose, Purpose::Recover) {
            self.held_since = Some(Instant::now());
        }
        self.rebuild = Some(Rebuild {
            handle,
            since: Instant::now(),
            purpose,
        });
        Ok(())
    }

    /// Swap a finished rebuild in. `Ok(false)`: one is still running.
    fn finish_rebuild(&mut self) -> Result<bool> {
        if let Some(msg) = &self.dead {
            return Err(anyhow!("{msg}"));
        }
        let Some(r) = &self.rebuild else {
            return Ok(true);
        };
        if !r.handle.is_finished() {
            if r.since.elapsed() < r.deadline() {
                return Ok(false);
            }
            // A hung driver (TDR, reset): give up so the ticker falls back
            // to the CPU. Dropping the handle detaches the thread.
            let d = r.deadline();
            self.rebuild = None;
            tracing::warn!("GPU device build still running after {d:?}; giving up");
            return Err(self.die(format!("the GPU device could not be created within {d:?}")));
        }
        let r = self.rebuild.take().expect("checked above");
        let built = r
            .handle
            .join()
            .map_err(|_| anyhow!("GPU device rebuild panicked"))
            .and_then(|b| b);
        match (built, r.purpose) {
            (Ok(p), Purpose::Recover) => {
                self.producer = p;
                tracing::info!("GPU device recreated");
            }
            (Ok(p), Purpose::Retarget(a)) => {
                self.producer = p;
                self.luid = a.luid;
                tracing::warn!(
                    adapter = %a.name,
                    "the video now runs on {}, the GPU Chromium chose",
                    a.name
                );
                if let Some(f) = &mut self.on_adapter_change {
                    f(&a);
                }
            }
            (Err(e), Purpose::Recover) => {
                return Err(self.die(format!(
                    "recreating the GPU device after device loss: {e:#}"
                )));
            }
            (Err(e), Purpose::Retarget(a)) => {
                return Err(self.die(format!(
                    "moving the video to {}, the GPU Chromium renders on: {e:#}",
                    a.name
                )));
            }
        }
        Ok(true)
    }

    /// Chromium's textures open only on another adapter: start moving there
    /// (once).
    fn check_adapter_mismatch(&mut self) -> Result<()> {
        if self.retargeted || self.rebuild.is_some() {
            return Ok(());
        }
        let Some(luid) = self.holder.current().and_then(|p| p.adapter_mismatch()) else {
            return Ok(());
        };
        self.retargeted = true;
        // LUIDs are never kept: look the adapter up in a fresh list.
        let Some(a) = crate::enumerate()
            .into_iter()
            .find(|a| a.luid == luid && !a.software)
        else {
            return Err(self.die(format!(
                "Chromium renders on a GPU that is gone (LUID {luid:#x})"
            )));
        };
        tracing::warn!(
            chromium = %a.name,
            "Chromium ignored the GPU switch and renders on {}; moving the video there",
            a.name
        );
        self.start_rebuild(Purpose::Retarget(a))
    }
}

impl OutputProducer for RecoveringProducer {
    fn produce(&mut self, inputs: &TickInputs, preview_due: bool) -> Result<Option<Output>> {
        if !self.finish_rebuild()? {
            if matches!(
                self.rebuild.as_ref().map(|r| &r.purpose),
                Some(Purpose::Recover)
            ) {
                return Ok(None);
            }
            // Moving adapters: the old device keeps going meanwhile.
        }
        self.check_adapter_mismatch()?;
        let held = self.rebuild.is_none().then(|| self.held_since.take());
        match self.producer.produce(inputs, preview_due) {
            Err(e) if is_device_lost(&e) && !self.recreated && self.rebuild.is_none() => {
                self.recreated = true;
                tracing::warn!(error = %format!("{e:#}"), "GPU device lost; recreating it once");
                self.start_rebuild(Purpose::Recover)?;
                Err(e.context("GPU device lost (recreating it)"))
            }
            r => {
                if r.is_err() {
                    // Still frozen since the hold began, if there was one.
                    if let Some(Some(t)) = held {
                        self.held_since = Some(t);
                    }
                }
                r
            }
        }
    }

    fn name(&self) -> String {
        "GPU".into()
    }

    fn pipeline_frames(&self) -> u32 {
        self.producer.pipeline_frames()
    }

    fn holding_since(&self) -> Option<Instant> {
        self.held_since
    }
}
