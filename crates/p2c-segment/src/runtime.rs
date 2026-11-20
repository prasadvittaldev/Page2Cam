//! Loading ONNX Runtime at run time (nothing is linked at build time, so the
//! Windows cross-build needs no import library) and opening model sessions.
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Context;
use ort::environment::Environment;
use ort::session::Session;

static ENV: std::sync::OnceLock<Environment> = std::sync::OnceLock::new();

/// The environment created by [`init`].
pub fn env() -> anyhow::Result<&'static Environment> {
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
    let hint = if cfg!(windows) {
        " (if this file is present, run install-camera.cmd as administrator: it installs the Microsoft Visual C++ runtime it needs)"
    } else {
        ""
    };
    let env = ort::init_from(&lib)
        .with_context(|| format!("loading {}{hint}", lib.display()))?
        .with_name("page2cam")
        .build()
        .with_context(|| format!("starting {}", lib.display()))?;
    let _ = ENV.set(env);
    Ok(())
}

/// How much work one inference is, for sizing the CPU thread pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelSize {
    /// About a millisecond (MediaPipe selfie): threads cost more than they save.
    Small,
    /// Tens of milliseconds on the CPU (RVM).
    Large,
}

/// Threads for one CPU inference. ONNX Runtime's default is one per core,
/// spinning while idle: on a 20-core laptop that burnt ~6 cores for a 1 ms
/// model and slowed it down (threads waiting on each other and on E-cores),
/// while competing with the browser and the compositor.
pub fn cpu_threads(size: ModelSize, available: usize) -> usize {
    match size {
        ModelSize::Small => available.clamp(1, 2),
        ModelSize::Large => (available / 2).clamp(1, 4),
    }
}

/// Threads for the nodes a DirectML session still runs on the CPU (shape
/// glue, input/output copies). ONNX Runtime's default is one spinning thread
/// per core (28 on the user's laptop) while the GPU does the work. One
/// sleeping thread measured within 2 % of the default on RVM (2026-11-20).
pub fn dml_threads(available: usize) -> usize {
    available.clamp(1, 1)
}

/// Open `model`. With `gpu` (a DXGI plain-order adapter index) on Windows,
/// DirectML is tried first on that adapter and the CPU is used if that fails.
/// `None` means CPU only. Returns the device actually in use.
pub fn session(
    model: &Path,
    gpu: Option<u32>,
    size: ModelSize,
) -> anyhow::Result<(Session, Device)> {
    anyhow::ensure!(model.is_file(), "model missing: {}", model.display());
    #[cfg(windows)]
    if let Some(index) = gpu {
        // DirectML requires memory pattern and parallel execution off
        // ("or an error is returned"), and must be the EP we asked for.
        // The adapter is named by its DXGI plain-order index (see `p2c_d3d`).
        let ep = ort::ep::DirectML::default().with_device_id(index as i32);
        let threads = dml_threads(std::thread::available_parallelism().map_or(1, |n| n.get()));
        let dml = Session::builder(env()?)
            .map_err(anyhow::Error::from)
            .and_then(|b| {
                b.with_intra_threads(threads)
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
            .and_then(|b| b.with_inter_threads(1).map_err(|e| anyhow::anyhow!("{e}")))
            // Idle threads sleep instead of spinning between frames.
            .and_then(|b| {
                b.with_intra_op_spinning(false)
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
            .and_then(|b| {
                b.with_inter_op_spinning(false)
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
            .and_then(|b| {
                b.with_memory_pattern(false)
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
            .and_then(|b| {
                b.with_parallel_execution(false)
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
            .and_then(|b| {
                b.with_execution_providers([ep.build().error_on_failure()])
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
            .and_then(|mut b| b.commit_from_file(model).map_err(anyhow::Error::from));
        match dml {
            Ok(s) => {
                let name = p2c_d3d::enumerate()
                    .into_iter()
                    .find(|a| a.index == index)
                    .map_or_else(|| "unknown".to_string(), |a| a.name);
                let gpu = format!("{name} (adapter {index})");
                tracing::info!(model = %model.display(), %gpu, threads, "running on the GPU (DirectML)");
                return Ok((s, Device::DirectMl));
            }
            Err(e) => tracing::warn!("DirectML unavailable, using CPU: {e:#}"),
        }
    }
    #[cfg(not(windows))]
    let _ = gpu;
    let available = std::thread::available_parallelism().map_or(1, |n| n.get());
    let threads = cpu_threads(size, available);
    // CPU on purpose: stop ONNX Runtime's automatic device policy from
    // picking another device behind our back (so the status stays truthful).
    let s = Session::builder(env()?)?
        .with_auto_device(ort::session::builder::AutoDevicePolicy::PreferCPU)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_intra_threads(threads)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_inter_threads(1)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        // Idle threads sleep instead of spinning on a core between frames.
        .with_intra_op_spinning(false)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_inter_op_spinning(false)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .commit_from_file(model)
        .with_context(|| format!("opening {}", model.display()))?;
    tracing::info!(model = %model.display(), threads, "running on the CPU");
    Ok((s, Device::Cpu))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_models_use_few_threads() {
        assert_eq!(cpu_threads(ModelSize::Small, 28), 2);
        assert_eq!(cpu_threads(ModelSize::Small, 1), 1);
    }

    #[test]
    fn directml_keeps_cpu_nodes_to_few_sleeping_threads() {
        assert_eq!(dml_threads(28), 1);
        assert_eq!(dml_threads(1), 1);
    }

    #[test]
    fn large_models_use_some_cores_but_not_all() {
        assert_eq!(cpu_threads(ModelSize::Large, 28), 4);
        assert_eq!(cpu_threads(ModelSize::Large, 6), 3);
        assert_eq!(cpu_threads(ModelSize::Large, 1), 1);
    }
}
