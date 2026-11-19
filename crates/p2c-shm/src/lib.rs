//! Frame hand-off between the Page2Cam app and its Windows camera DLL.
//!
//! The DLL (running inside the Windows Frame Server service) creates a named
//! file mapping; the app writes frames into it with a seqlock (NV12, BT.709
//! limited range, from the Windows app; BGRA is still understood); the DLL
//! copies the newest consistent frame into each camera sample.
mod convert;
mod layout;

pub use convert::{fill_nv12, fill_rgb32, nv12_to_nv12, nv12_to_rgb32};
pub use layout::{
    dll_version, read_frame, set_dll_version, set_writer, write_frame, writer, FrameInfo,
    PixelFormat, ReadError, StaleState, Writer, HEADER_SIZE, MAPPING_SIZE, MAX_HEIGHT, MAX_WIDTH,
    VERSION,
};

/// Name of the shared mapping (the Global namespace spans sessions).
pub const MAPPING_NAME: &str = "Global\\Page2CamFrames";
/// COM class id of the Page2Cam camera media source.
pub const CLSID_STR: &str = "{7C2F9A41-3B6E-4D8A-9C1F-5E2B8D4A6F10}";
/// Friendly name apps show in their camera list.
pub const CAMERA_NAME: &str = "Page2Cam";
