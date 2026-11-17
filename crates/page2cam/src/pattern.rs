//! Moving-bar test pattern, used to check the camera path without a browser.
use std::time::Duration;

use p2c_core::{Frame, FrameSlot};

/// Spawn a thread that writes a frame about every 33 ms: a white vertical
/// bar moving across a dark-gray background.
pub fn spawn(slot: FrameSlot, w: u32, h: u32) {
    std::thread::Builder::new()
        .name("p2c-pattern".into())
        .spawn(move || {
            let bar = (w / 20).max(2);
            let mut x0 = 0u32;
            loop {
                let mut f = Frame::new_black(w, h);
                for (i, px) in f.data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let x = i as u32 % w;
                    let v = if x >= x0 && x < x0 + bar { 255 } else { 40 };
                    *px = [v, v, v, 255];
                }
                slot.put(f);
                x0 = (x0 + w / 90 + 1) % w;
                std::thread::sleep(Duration::from_millis(33));
            }
        })
        .expect("spawn pattern thread");
}
