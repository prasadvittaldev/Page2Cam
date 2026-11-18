//! Webcam cut-out: runs a person-segmentation model on webcam frames and
//! writes the mask into each frame's alpha byte (see `tensor::write_alpha`).
use std::path::PathBuf;

pub mod engine;
pub mod policy;
pub mod runtime;
pub mod segmenter;
pub mod tensor;

pub use engine::{open_engine, Engine, EngineChoice};
pub use runtime::Device;
pub use segmenter::{Opener, SegStatus, Segmenter};

/// Folder holding `models/` and `onnxruntime/`: next to the exe when shipped,
/// else the repo's `third_party/` (dev builds and tests).
pub fn assets_dir() -> PathBuf {
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()))
    {
        if dir.join("models").is_dir() {
            return dir;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party")
}

/// The `models` folder.
pub fn models_dir() -> PathBuf {
    assets_dir().join("models")
}

/// Folder holding the ONNX Runtime library: next to the exe when shipped
/// (the Windows zip), else `third_party/onnxruntime/<os>`.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()))
    {
        if runtime::library_path(&dir).is_file() {
            return dir;
        }
    }
    assets_dir()
        .join("onnxruntime")
        .join(if cfg!(windows) { "windows" } else { "linux" })
}
