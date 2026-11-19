//! Real webcam capture into a FrameSlot.
mod capture;
mod decode;
mod format;
mod pick;

pub use capture::{list_cameras, WebcamCapture, WebcamStatus};
pub use format::{choose_format, describe_modes};
pub use pick::{pick_camera, CameraInfo};
