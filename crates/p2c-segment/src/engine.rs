//! Segmentation engines: Quality (Robust Video Matting) and Fast (MediaPipe selfie).
use std::fmt;
use std::path::Path;

use anyhow::Context;
use ort::session::Session;
use ort::value::{DynValue, Tensor};
use std::time::{Duration, Instant};

use p2c_core::{Frame, Nv12Frame};

use crate::tensor::{nv12_can_feed, nv12_to_frame, nv12_to_rgb_f32_nchw, to_rgb_f32_nchw};
use crate::{runtime, Device};

/// Model files inside the `models` folder.
pub const RVM_MODEL: &str = "rvm_mobilenetv3_fp32.onnx";
pub const SELFIE_MODEL: &str = "selfie_landscape.onnx";

/// Which cut-out engine the operator asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EngineChoice {
    #[default]
    Quality,
    Fast,
}

impl EngineChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            EngineChoice::Quality => "quality",
            EngineChoice::Fast => "fast",
        }
    }

    pub fn parse(s: &str) -> Option<EngineChoice> {
        match s {
            "quality" => Some(EngineChoice::Quality),
            "fast" => Some(EngineChoice::Fast),
            _ => None,
        }
    }
}

impl fmt::Display for EngineChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EngineChoice::Quality => "Quality",
            EngineChoice::Fast => "Fast",
        })
    }
}

/// A person-segmentation model.
pub trait Engine: Send {
    /// Mask for `frame`: values 0..1 (1 = person), row-major, with its size.
    fn infer(&mut self, frame: &Frame) -> anyhow::Result<(Vec<f32>, u32, u32)>;
    /// Like `infer`, for an NV12 frame. The real engines build their input
    /// straight from NV12; the default converts to BGRA first.
    fn infer_nv12(&mut self, frame: &Nv12Frame) -> anyhow::Result<(Vec<f32>, u32, u32)> {
        self.infer(&nv12_to_frame(frame))
    }
    /// Time the last `infer*` spent turning the frame into the model input
    /// (taken once), if the engine measures it.
    fn take_prep(&mut self) -> Option<Duration> {
        None
    }
    /// Forget anything carried between frames.
    fn reset(&mut self);
    fn device(&self) -> Device;
}

/// Open the real engine for `choice` from the `models` folder `dir`. `gpu` is
/// the DXGI adapter index for Quality (DirectML); `None` keeps it on the CPU.
/// Fast always runs on the CPU.
pub fn open_engine(
    dir: &Path,
    choice: EngineChoice,
    gpu: Option<u32>,
) -> anyhow::Result<Box<dyn Engine>> {
    Ok(match choice {
        EngineChoice::Quality => Box::new(RvmEngine::open(dir, gpu)?),
        EngineChoice::Fast => Box::new(FastEngine::open(dir)?),
    })
}

/// MediaPipe selfie segmentation (landscape): 256x144 RGB in, 256x144 mask out.
pub struct FastEngine {
    session: Session,
    device: Device,
    prep: Option<Duration>,
}

/// Model input (`w` x `h`) from either kind of frame, and how long it took.
fn model_input(frame: Input, w: u32, h: u32) -> (Vec<f32>, Duration) {
    let t = Instant::now();
    let v = match frame {
        Input::Bgra(f) => to_rgb_f32_nchw(f, w, h),
        Input::Nv12(n) if nv12_can_feed(n, w, h) => nv12_to_rgb_f32_nchw(n, w, h),
        Input::Nv12(n) => to_rgb_f32_nchw(&nv12_to_frame(n), w, h),
    };
    (v, t.elapsed())
}

#[derive(Clone, Copy)]
enum Input<'a> {
    Bgra(&'a Frame),
    Nv12(&'a Nv12Frame),
}

const FAST_W: u32 = 256;
const FAST_H: u32 = 144;

impl FastEngine {
    pub fn open(dir: &Path) -> anyhow::Result<FastEngine> {
        // Tiny model: the CPU is fast enough and avoids GPU upload overhead.
        let (session, device) =
            runtime::session(&dir.join(SELFIE_MODEL), None, runtime::ModelSize::Small)?;
        Ok(FastEngine {
            session,
            device,
            prep: None,
        })
    }

    fn run(&mut self, frame: Input) -> anyhow::Result<(Vec<f32>, u32, u32)> {
        let (input, prep) = model_input(frame, FAST_W, FAST_H);
        self.prep = Some(prep);
        let tensor = Tensor::from_array(([1usize, 3, FAST_H as usize, FAST_W as usize], input))?;
        let out = self.session.run(ort::inputs!["pixel_values" => tensor])?;
        let (_, mask) = out["alphas"]
            .try_extract_tensor::<f32>()
            .context("reading the selfie mask")?;
        Ok((mask.to_vec(), FAST_W, FAST_H))
    }
}

impl Engine for FastEngine {
    fn infer(&mut self, frame: &Frame) -> anyhow::Result<(Vec<f32>, u32, u32)> {
        self.run(Input::Bgra(frame))
    }

    fn infer_nv12(&mut self, frame: &Nv12Frame) -> anyhow::Result<(Vec<f32>, u32, u32)> {
        self.run(Input::Nv12(frame))
    }

    fn take_prep(&mut self) -> Option<Duration> {
        self.prep.take()
    }

    fn reset(&mut self) {}

    fn device(&self) -> Device {
        self.device
    }
}

/// Robust Video Matting (MobileNetV3). Carries four recurrent states between
/// frames, which is what keeps the edge steady.
pub struct RvmEngine {
    session: Session,
    device: Device,
    /// The last run's `r1o`..`r4o`, fed back by reference as `r1i`..`r4i`
    /// (no copy); `None` until the first run and after a reset.
    rec: Option<[DynValue; 4]>,
    prep: Option<Duration>,
}

/// RVM input size and its internal downsample ratio (640x360 * 0.4 = 256x144
/// for the coarse pass; the refiner works at 640x360).
const RVM_W: u32 = 640;
const RVM_H: u32 = 360;
const RVM_DOWNSAMPLE: f32 = 0.4;

/// The empty recurrent state RVM starts from: a 1x1x1x1 zero.
fn empty_rec() -> anyhow::Result<[DynValue; 4]> {
    let z = || -> anyhow::Result<DynValue> {
        Ok(Tensor::from_array(([1usize, 1, 1, 1], vec![0.0f32]))?.into_dyn())
    };
    Ok([z()?, z()?, z()?, z()?])
}

impl RvmEngine {
    pub fn open(dir: &Path, gpu: Option<u32>) -> anyhow::Result<RvmEngine> {
        let (session, device) =
            runtime::session(&dir.join(RVM_MODEL), gpu, runtime::ModelSize::Large)?;
        Ok(RvmEngine {
            session,
            device,
            rec: None,
            prep: None,
        })
    }

    fn run(&mut self, frame: Input) -> anyhow::Result<(Vec<f32>, u32, u32)> {
        let (src, prep) = model_input(frame, RVM_W, RVM_H);
        self.prep = Some(prep);
        let rec = match self.rec.take() {
            Some(r) => r,
            None => empty_rec()?,
        };
        let result = Self::step(&mut self.session, src, &rec);
        // A failed run keeps the state it started from, as before.
        let (mask, next) = match result {
            Ok(r) => r,
            Err(e) => {
                self.rec = Some(rec);
                return Err(e);
            }
        };
        self.rec = Some(next);
        Ok((mask, RVM_W, RVM_H))
    }

    /// One inference: the mask and the next recurrent state, taken out of
    /// the outputs as owned values (no copy).
    fn step(
        session: &mut Session,
        src: Vec<f32>,
        rec: &[DynValue; 4],
    ) -> anyhow::Result<(Vec<f32>, [DynValue; 4])> {
        let inputs = ort::inputs![
            "src" => Tensor::from_array(([1usize, 3, RVM_H as usize, RVM_W as usize], src))?,
            "r1i" => &rec[0],
            "r2i" => &rec[1],
            "r3i" => &rec[2],
            "r4i" => &rec[3],
            "downsample_ratio" => Tensor::from_array(([1usize], vec![RVM_DOWNSAMPLE]))?,
        ];
        let mut out = session.run(inputs)?;
        let mask = out["pha"]
            .try_extract_tensor::<f32>()
            .context("reading the RVM mask")?
            .1
            .to_vec();
        let mut next = |name: &str| {
            out.remove(name)
                .ok_or_else(|| anyhow::anyhow!("RVM output {name} missing"))
        };
        Ok((
            mask,
            [next("r1o")?, next("r2o")?, next("r3o")?, next("r4o")?],
        ))
    }
}

impl Engine for RvmEngine {
    fn infer(&mut self, frame: &Frame) -> anyhow::Result<(Vec<f32>, u32, u32)> {
        self.run(Input::Bgra(frame))
    }

    fn infer_nv12(&mut self, frame: &Nv12Frame) -> anyhow::Result<(Vec<f32>, u32, u32)> {
        self.run(Input::Nv12(frame))
    }

    fn take_prep(&mut self) -> Option<Duration> {
        self.prep.take()
    }

    fn reset(&mut self) {
        self.rec = None;
    }

    fn device(&self) -> Device {
        self.device
    }
}
