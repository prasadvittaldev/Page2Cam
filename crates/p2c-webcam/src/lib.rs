//! Real webcam capture into a FrameSlot.
mod capture;
mod decode;
mod pick;

pub use capture::{list_cameras, WebcamCapture};
pub use pick::{pick_camera, CameraInfo};
