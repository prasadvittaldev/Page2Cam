//! Page2Cam's Windows 11 virtual camera: a Media Foundation media source COM
//! object. Windows' Frame Server service loads this DLL when an app opens the
//! "Page2Cam" camera (created by page2cam.exe via MFCreateVirtualCamera).
//!
//! Ported from smourier/VCamSample (MIT): same interface set and stream logic,
//! CPU samples only, frames read from page2cam's shared memory (p2c-shm).
#![allow(non_snake_case)]
#[cfg(windows)]
mod win;
