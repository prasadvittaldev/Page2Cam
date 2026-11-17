//! CEF integration tests. Need CEF_PATH + LD_LIBRARY_PATH (see CLAUDE.md).
//! Run: cargo test -p p2c-browser --test render -- --ignored
use std::path::PathBuf;
use std::time::{Duration, Instant};

use p2c_browser::{BrowserConfig, BrowserHandle};
use p2c_core::{Frame, FrameSlot};

const W: u32 = 640;
const H: u32 = 360;

fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter()
        .zip(b)
        .all(|(x, y)| (*x as i32 - y as i32).abs() <= 3)
}

/// Run CEF on this (main) thread; a watcher thread polls `check` on each new frame
/// until it returns true or 10 s pass, then shuts the browser down.
fn run_until(html: &str, mut check: impl FnMut(&Frame) -> bool + Send + 'static) -> bool {
    let slot = FrameSlot::new();
    let profile: PathBuf =
        std::env::temp_dir().join(format!("p2c-test-profile-{}", std::process::id()));
    let cfg = BrowserConfig {
        url: format!("data:text/html,{html}"),
        width: W,
        height: H,
        fps: 30,
        profile_dir: profile,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let watch = slot.clone();
    p2c_browser::run(cfg, slot, move |h: BrowserHandle| {
        std::thread::spawn(move || {
            let start = Instant::now();
            let mut ok = false;
            while start.elapsed() < Duration::from_secs(10) {
                if let Some(f) = watch.latest() {
                    if f.width == W && f.height == H && check(&f) {
                        ok = true;
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            tx.send(ok).unwrap();
            h.shutdown();
        });
    })
    .expect("cef run");
    rx.recv().unwrap()
}

fn main() {
    if let Some(code) = p2c_browser::maybe_run_subprocess() {
        std::process::exit(code);
    }
    if !std::env::args().any(|a| a == "--ignored" || a == "--include-ignored") {
        println!("render tests ignored (need CEF); pass --ignored to run");
        return;
    }
    // CEF can be initialized only once per process, so both checks share one page:
    // left half green, right half turns blue after 500 ms (a partial repaint).
    let html = "<body style='margin:0'><div style='display:flex;height:100vh'>\
        <div style='flex:1;background:%2300ff00'></div>\
        <div id=r style='flex:1;background:%23ff0000'></div></div>\
        <script>setTimeout(()=>r.style.background='%230000ff',500)</script></body>";
    let mut seen_red = false;
    let ok = run_until(html, move |f| {
        // renders_solid_color: right half is red before the change.
        if close(f.pixel(540, 180), [0, 0, 255, 255]) {
            seen_red = true;
        }
        // partial_repaint_keeps_full_frame: after the change, left is still green.
        seen_red
            && close(f.pixel(540, 180), [255, 0, 0, 255])
            && close(f.pixel(100, 180), [0, 255, 0, 255])
    });
    if ok {
        println!("test renders_solid_color ... ok\ntest partial_repaint_keeps_full_frame ... ok");
    } else {
        eprintln!("test renders_solid_color / partial_repaint_keeps_full_frame ... FAILED");
        std::process::exit(1);
    }
}
