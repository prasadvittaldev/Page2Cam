//! Direct3D 11 support for the Windows GPU pipeline. Pure types and logic
//! compile everywhere; DXGI calls are Windows-only.
pub mod adapters;

pub use adapters::{choose, enumerate, AdapterInfo, GpuChoice, GpuSetting};
