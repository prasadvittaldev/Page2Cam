#![cfg(target_os = "linux")]
use std::path::Path;

use p2c_compositor::{FrameSink, OutputSpec};
use p2c_core::Frame;
use p2c_vcam::{find_loopback_device, V4l2Sink};

/// Requires: sudo modprobe v4l2loopback exclusive_caps=1 card_label=Page2Cam
#[test]
#[ignore]
fn writes_frames() {
    let dev = find_loopback_device(Path::new("/sys")).expect("no v4l2loopback device loaded");
    let spec = OutputSpec {
        width: 1280,
        height: 720,
        fps: 30,
    };
    let mut sink = V4l2Sink::open(&dev, &spec).unwrap();
    let frame = Frame::new_black(1280, 720);
    for _ in 0..30 {
        sink.push(&frame).unwrap();
    }
}
