//! CEF integration tests. Need CEF_PATH (see CLAUDE.md).
//! Run: cargo test -p p2c-browser --test render -- --ignored
//!
//! CEF can be initialized only once per process, so all checks run as steps of
//! one browser session, driven from a watcher thread.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use p2c_browser::{BrowserConfig, BrowserHandle, KeyInput, KeyKind, MouseButton};
use p2c_core::{AudioRing, Frame, FrameSlot};

const W: u32 = 640;
const H: u32 = 360;
const RED: [u8; 4] = [0, 0, 255, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [255, 0, 0, 255];
const YELLOW: [u8; 4] = [0, 255, 255, 255];
const MAGENTA: [u8; 4] = [255, 0, 255, 255];

fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter()
        .zip(b)
        .all(|(x, y)| (*x as i32 - y as i32).abs() <= 3)
}

fn wait_for(slot: &FrameSlot, what: &str, check: impl Fn(&Frame) -> bool) -> Result<(), String> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if let Some(f) = slot.latest() {
            if f.width == W && f.height == H && check(&f) {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("timed out waiting for {what}"))
}

/// Left half green; right half red, turning blue after 500 ms (partial repaint);
/// clicking the right half turns it yellow; a quiet 440 Hz tone plays.
const PAGE: &str = "<body style='margin:0'><div style='display:flex;height:100vh'>\
    <div style='flex:1;background:%2300ff00'></div>\
    <div id=r style='flex:1;background:%23ff0000' onclick=\"this.style.background='%23ffff00'\"></div></div>\
    <script>setTimeout(()=>r.style.background='%230000ff',500);\
    const c=new AudioContext(),o=c.createOscillator(),g=c.createGain();g.gain.value=0.2;\
    o.connect(g).connect(c.destination);o.start();\
    document.onkeypress=e=>{if(e.key==='k')r.style.background='%23ff00ff'};</script></body>";

fn scenario(h: &BrowserHandle, slot: &FrameSlot, audio: &AudioRing) -> Result<(), String> {
    wait_for(slot, "red right half", |f| close(f.pixel(540, 180), RED))?;
    println!("test renders_solid_color ... ok");
    wait_for(slot, "blue right half with green left", |f| {
        close(f.pixel(540, 180), BLUE) && close(f.pixel(100, 180), GREEN)
    })?;
    println!("test partial_repaint_keeps_full_frame ... ok");

    h.mouse_move(540, 180, 0);
    h.mouse_button(540, 180, MouseButton::Left, true, 1, 0);
    h.mouse_button(540, 180, MouseButton::Left, false, 1, 0);
    wait_for(slot, "yellow after click", |f| {
        close(f.pixel(540, 180), YELLOW)
    })?;
    println!("test click_reaches_page ... ok");

    let k = |kind, code, ch| KeyInput {
        kind,
        windows_key_code: code,
        character: ch,
        modifiers: 0,
    };
    h.key(k(KeyKind::Down, 0x4B, 0));
    h.key(k(KeyKind::Char, 0x4B, b'k' as u16));
    h.key(k(KeyKind::Up, 0x4B, 0));
    wait_for(slot, "magenta after typing k", |f| {
        close(f.pixel(540, 180), MAGENTA)
    })?;
    println!("test keys_reach_page ... ok");

    let start = Instant::now();
    let mut buf = vec![0.0f32; 4800];
    let mut loud = false;
    while start.elapsed() < Duration::from_secs(10) && !loud {
        let n = audio.pop_into(&mut buf);
        loud = buf[..n].iter().any(|s| s.abs() > 0.05);
        std::thread::sleep(Duration::from_millis(50));
    }
    if !loud {
        return Err("no page audio captured".into());
    }
    println!("test page_audio_captured ... ok");

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) && !h.current_url().starts_with("data:") {
        std::thread::sleep(Duration::from_millis(50));
    }
    if !h.current_url().starts_with("data:") {
        return Err(format!("current_url = {:?}", h.current_url()));
    }
    println!("test current_url_tracks_page ... ok");
    Ok(())
}

fn main() {
    if let Some(code) = p2c_browser::maybe_run_subprocess() {
        std::process::exit(code);
    }
    if !std::env::args().any(|a| a == "--ignored" || a == "--include-ignored") {
        println!("render tests ignored (need CEF); pass --ignored to run");
        return;
    }
    let slot = FrameSlot::new();
    let audio = AudioRing::new(2000);
    let profile: PathBuf =
        std::env::temp_dir().join(format!("p2c-test-profile-{}", std::process::id()));
    let cfg = BrowserConfig {
        url: format!("data:text/html,{PAGE}"),
        width: W,
        height: H,
        fps: 30,
        profile_dir: profile.clone(),
        audio: Some(audio.clone()),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let watch = slot.clone();
    p2c_browser::run(cfg, slot, move |h: BrowserHandle| {
        std::thread::spawn(move || {
            tx.send(scenario(&h, &watch, &audio)).unwrap();
            h.shutdown();
        });
    })
    .expect("cef run");
    let _ = std::fs::remove_dir_all(&profile);
    if let Err(e) = rx.recv().unwrap() {
        eprintln!("render test FAILED: {e}");
        std::process::exit(1);
    }
}
