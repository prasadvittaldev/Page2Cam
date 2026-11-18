//! Segmentation engines: Quality (Robust Video Matting) and Fast (MediaPipe selfie).
use std::fmt;
use std::path::Path;

use anyhow::Context;
use ort::session::Session;
use ort::value::Tensor;
use p2c_core::Frame;

use crate::tensor::to_rgb_f32_nchw;
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
    /// Forget anything carried between frames.
    fn reset(&mut self);
    fn device(&self) -> Device;
}

/// Open the real engine for `choice` from the `models` folder `dir`.
pub fn open_engine(dir: &Path, choice: EngineChoice) -> anyhow::Result<Box<dyn Engine>> {
    Ok(match choice {
        EngineChoice::Quality => Box::new(RvmEngine::open(dir)?),
        EngineChoice::Fast => Box::new(FastEngine::open(dir)?),
    })
}

/// MediaPipe selfie segmentation (landscape): 256x144 RGB in, 256x144 mask out.
pub struct FastEngine {
    session: Session,
    device: Device,
}

const FAST_W: u32 = 256;
const FAST_H: u32 = 144;

impl FastEngine {
    pub fn open(dir: &Path) -> anyhow::Result<FastEngine> {
        // Tiny model: the CPU is fast enough and avoids GPU upload overhead.
        let (session, device) = runtime::session(&dir.join(SELFIE_MODEL), false)?;
        Ok(FastEngine { session, device })
    }
}

impl Engine for FastEngine {
    fn infer(&mut self, frame: &Frame) -> anyhow::Result<(Vec<f32>, u32, u32)> {
        let input = to_rgb_f32_nchw(frame, FAST_W, FAST_H);
        let tensor = Tensor::from_array(([1usize, 3, FAST_H as usize, FAST_W as usize], input))?;
        let out = self.session.run(ort::inputs!["pixel_values" => tensor])?;
        let (_, mask) = out["alphas"]
            .try_extract_tensor::<f32>()
            .context("reading the selfie mask")?;
        Ok((mask.to_vec(), FAST_W, FAST_H))
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
    rec: [(Vec<i64>, Vec<f32>); 4],
}

/// RVM input size and its internal downsample ratio (640x360 * 0.4 = 256x144
/// for the coarse pass; the refiner works at 640x360).
const RVM_W: u32 = 640;
const RVM_H: u32 = 360;
const RVM_DOWNSAMPLE: f32 = 0.4;

fn empty_rec() -> [(Vec<i64>, Vec<f32>); 4] {
    std::array::from_fn(|_| (vec![1, 1, 1, 1], vec![0.0]))
}

impl RvmEngine {
    pub fn open(dir: &Path) -> anyhow::Result<RvmEngine> {
        let (session, device) = runtime::session(&dir.join(RVM_MODEL), true)?;
        Ok(RvmEngine {
            session,
            device,
            rec: empty_rec(),
        })
    }
}

impl Engine for RvmEngine {
    fn infer(&mut self, frame: &Frame) -> anyhow::Result<(Vec<f32>, u32, u32)> {
        let src = to_rgb_f32_nchw(frame, RVM_W, RVM_H);
        let rec = |i: usize| -> anyhow::Result<Tensor<f32>> {
            let (shape, data) = &self.rec[i];
            Ok(Tensor::from_array((shape.clone(), data.clone()))?)
        };
        let inputs = ort::inputs![
            "src" => Tensor::from_array(([1usize, 3, RVM_H as usize, RVM_W as usize], src))?,
            "r1i" => rec(0)?,
            "r2i" => rec(1)?,
            "r3i" => rec(2)?,
            "r4i" => rec(3)?,
            "downsample_ratio" => Tensor::from_array(([1usize], vec![RVM_DOWNSAMPLE]))?,
        ];
        let out = self.session.run(inputs)?;
        let mut next = empty_rec();
        for (i, name) in ["r1o", "r2o", "r3o", "r4o"].into_iter().enumerate() {
            let (shape, data) = out[name].try_extract_tensor::<f32>()?;
            next[i] = (shape.to_vec(), data.to_vec());
        }
        let (_, pha) = out["pha"]
            .try_extract_tensor::<f32>()
            .context("reading the RVM mask")?;
        let mask = pha.to_vec();
        drop(out);
        self.rec = next;
        Ok((mask, RVM_W, RVM_H))
    }

    fn reset(&mut self) {
        self.rec = empty_rec();
    }

    fn device(&self) -> Device {
        self.device
    }
}
