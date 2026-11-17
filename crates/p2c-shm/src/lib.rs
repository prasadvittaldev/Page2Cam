//! Frame hand-off between the Page2Cam app and its Windows camera DLL.
//!
//! The DLL (running inside the Windows Frame Server service) creates a named
//! file mapping; the app writes BGRA frames into it with a seqlock; the DLL
//! copies the newest consistent frame into each camera sample.
mod convert;
mod layout;

pub use convert::{fill_nv12, fill_rgb32};
pub use layout::{
    read_frame, write_frame, FrameInfo, HEADER_SIZE, MAPPING_SIZE, MAX_HEIGHT, MAX_WIDTH,
};

/// Name of the shared mapping (the Global namespace spans sessions).
pub const MAPPING_NAME: &str = "Global\\Page2CamFrames";
/// COM class id of the Page2Cam camera media source.
pub const CLSID_STR: &str = "{7C2F9A41-3B6E-4D8A-9C1F-5E2B8D4A6F10}";
/// Friendly name apps show in their camera list.
pub const CAMERA_NAME: &str = "Page2Cam";
