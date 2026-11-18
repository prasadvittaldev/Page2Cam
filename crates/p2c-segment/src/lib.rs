//! Webcam cut-out: runs a person-segmentation model on webcam frames and
//! writes the mask into each frame's alpha byte (see `tensor::write_alpha`).
use std::path::PathBuf;

pub mod runtime;
pub mod tensor;

pub use runtime::Device;

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
