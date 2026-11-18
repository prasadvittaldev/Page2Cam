//! Loading ONNX Runtime at run time (nothing is linked at build time, so the
//! Windows cross-build needs no import library) and opening model sessions.
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Context;
use ort::environment::Environment;
use ort::session::Session;

static ENV: std::sync::OnceLock<Environment> = std::sync::OnceLock::new();

fn env() -> anyhow::Result<&'static Environment> {
    ENV.get()
        .ok_or_else(|| anyhow::anyhow!("ONNX Runtime was not loaded"))
}

/// Where a model runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    DirectMl,
    Cpu,
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Device::DirectMl => "GPU (DirectML)",
            Device::Cpu => "CPU",
        })
    }
}

/// The ONNX Runtime library file inside `lib_dir`.
pub fn library_path(lib_dir: &Path) -> PathBuf {
    lib_dir.join(if cfg!(windows) {
        "onnxruntime.dll"
    } else {
        "libonnxruntime.so"
    })
}

/// Load ONNX Runtime from `lib_dir`. Call once, before [`session`].
pub fn init(lib_dir: &Path) -> anyhow::Result<()> {
    let lib = library_path(lib_dir);
    anyhow::ensure!(lib.is_file(), "ONNX Runtime not found: {}", lib.display());
    if ENV.get().is_some() {
        return Ok(());
    }
    let env = ort::init_from(&lib)
        .with_context(|| format!("loading {}", lib.display()))?
        .with_name("page2cam")
        .build()
        .with_context(|| format!("starting {}", lib.display()))?;
    let _ = ENV.set(env);
    Ok(())
}

/// Open `model`. With `prefer_gpu` on Windows, DirectML is tried first and
/// the CPU is used if that fails. Returns the device actually in use.
pub fn session(model: &Path, prefer_gpu: bool) -> anyhow::Result<(Session, Device)> {
    anyhow::ensure!(model.is_file(), "model missing: {}", model.display());
    #[cfg(windows)]
    if prefer_gpu {
        let dml = Session::builder(env()?)
            .map_err(anyhow::Error::from)
            .and_then(|b| {
                b.with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()])
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
            .and_then(|mut b| b.commit_from_file(model).map_err(anyhow::Error::from));
        match dml {
            Ok(s) => return Ok((s, Device::DirectMl)),
            Err(e) => tracing::warn!("DirectML unavailable, using CPU: {e:#}"),
        }
    }
    let _ = prefer_gpu;
    let s = Session::builder(env()?)?
        .commit_from_file(model)
        .with_context(|| format!("opening {}", model.display()))?;
    Ok((s, Device::Cpu))
}
