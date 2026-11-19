//! Direct3D 11 support for the Windows GPU pipeline. Pure types and logic
//! compile everywhere; DXGI/D3D11 calls are Windows-only.
pub mod adapters;
#[cfg(windows)]
mod backdrop;
#[cfg(windows)]
pub mod compose;
#[cfg(windows)]
pub mod device;
#[cfg(windows)]
pub mod page;
#[cfg(windows)]
mod profile;
#[cfg(windows)]
pub mod readback;
#[cfg(windows)]
pub mod recover;
#[cfg(windows)]
pub mod shaders;

pub use adapters::{choose, driver_version, enumerate, AdapterInfo, GpuChoice, GpuSetting};
#[cfg(windows)]
pub use compose::GpuProducer;
#[cfg(windows)]
pub use device::{is_device_lost, ContextLock, Gpu, SharedTexture, Texture};
#[cfg(windows)]
pub use page::{open_on_matching_adapter, PageInput};
#[cfg(windows)]
pub use readback::Readback;
#[cfg(windows)]
pub use recover::{PageHolder, RecoveringProducer};
#[cfg(windows)]
pub use shaders::{bgra_to_nv12_gpu, bgra_to_nv12_into};
