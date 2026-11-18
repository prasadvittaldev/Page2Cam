//! Webcam cut-out: runs a person-segmentation model on webcam frames and
//! publishes a cleaned-up person mask (see `post`).
use std::path::{Path, PathBuf};

pub mod engine;
pub mod policy;
pub mod post;
pub mod runtime;
pub mod segmenter;
pub mod tensor;

pub use engine::{open_engine, Engine, EngineChoice};
pub use runtime::Device;
pub use segmenter::{Opener, SegStatus, Segmenter};

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
}

/// The repo's `third_party/` in dev builds; None in release builds, so a
/// shipped exe only ever looks (and points the user) next to itself.
fn dev_dir() -> Option<PathBuf> {
    cfg!(debug_assertions)
        .then(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party"))
}

/// Folder holding `models/`: the exe folder if it has one, else (dev builds)
/// `dev`, else the exe folder (so errors name where the files belong).
pub fn assets_dir_in(exe_dir: Option<&Path>, dev: Option<&Path>) -> PathBuf {
    match (exe_dir, dev) {
        (Some(e), _) if e.join("models").is_dir() => e.to_path_buf(),
        (_, Some(d)) => d.to_path_buf(),
        (Some(e), None) => e.to_path_buf(),
        (None, None) => PathBuf::from("."),
    }
}

/// Folder holding the ONNX Runtime library: the exe folder if it has it,
/// else (dev builds) `dev/onnxruntime/<os>`, else the exe folder.
pub fn runtime_dir_in(exe_dir: Option<&Path>, dev: Option<&Path>) -> PathBuf {
    match (exe_dir, dev) {
        (Some(e), _) if runtime::library_path(e).is_file() => e.to_path_buf(),
        (_, Some(d)) => d
            .join("onnxruntime")
            .join(if cfg!(windows) { "windows" } else { "linux" }),
        (Some(e), None) => e.to_path_buf(),
        (None, None) => PathBuf::from("."),
    }
}

/// See [`assets_dir_in`].
pub fn assets_dir() -> PathBuf {
    assets_dir_in(exe_dir().as_deref(), dev_dir().as_deref())
}

/// The `models` folder.
pub fn models_dir() -> PathBuf {
    assets_dir().join("models")
}

/// See [`runtime_dir_in`].
pub fn runtime_dir() -> PathBuf {
    runtime_dir_in(exe_dir().as_deref(), dev_dir().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_build_never_points_at_build_tree() {
        let exe = tempfile_dir();
        assert_eq!(assets_dir_in(Some(&exe), None), exe);
        assert_eq!(runtime_dir_in(Some(&exe), None), exe);
    }

    #[test]
    fn exe_dir_wins_when_it_has_the_files() {
        let exe = tempfile_dir();
        std::fs::create_dir_all(exe.join("models")).unwrap();
        std::fs::write(runtime::library_path(&exe), b"x").unwrap();
        let dev = Path::new("/dev-tree");
        assert_eq!(assets_dir_in(Some(&exe), Some(dev)), exe);
        assert_eq!(runtime_dir_in(Some(&exe), Some(dev)), exe);
    }

    #[test]
    fn dev_build_falls_back_to_third_party() {
        let exe = tempfile_dir();
        let dev = Path::new("/dev-tree");
        assert_eq!(assets_dir_in(Some(&exe), Some(dev)), dev);
        assert_eq!(
            runtime_dir_in(Some(&exe), Some(dev)),
            dev.join("onnxruntime")
                .join(if cfg!(windows) { "windows" } else { "linux" })
        );
    }

    fn tempfile_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "p2c-seg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
