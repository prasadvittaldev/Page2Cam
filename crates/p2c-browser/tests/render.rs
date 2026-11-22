//! CEF integration tests. Need CEF_PATH (see CLAUDE.md).
//! Run: cargo test -p p2c-browser --test render -- --ignored
//!
//! CEF can be initialized only once per process, so all checks run as steps of
//! one browser session, driven from a watcher thread. On Windows with a
//! hardware GPU the session starts with shared textures (GPU path) and then
//! switches to CPU paints with `recreate_cpu`; the other steps run on CPU paints.
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
    wait_for_size(slot, (W, H), what, check)
}

fn wait_for_size(
    slot: &FrameSlot,
    (w, h): (u32, u32),
    what: &str,
    check: impl Fn(&Frame) -> bool,
) -> Result<(), String> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if let Some(f) = slot.latest() {
            if f.width == w && f.height == h && check(&f) {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("timed out waiting for {what}"))
}

/// Left half green; right half red, turning blue after 500 ms (partial repaint);
/// clicking the right half turns it yellow; a quiet 440 Hz tone plays. A tiny
/// flickering square in the top-left corner keeps frames coming.
const PAGE: &str = "<body style='margin:0'><div style='display:flex;height:100vh'>\
    <div style='flex:1;background:%2300ff00'></div>\
    <div id=r style='flex:1;background:%23ff0000' onclick=\"this.style.background='%23ffff00'\"></div></div>\
    <div id=q style='position:fixed;left:0;top:0;width:4px;height:4px;background:%23000'></div>\
    <script>setTimeout(()=>r.style.background='%230000ff',500);\
    (function f(){q.style.opacity=Math.random();requestAnimationFrame(f)})();\
    const c=new AudioContext(),o=c.createOscillator(),g=c.createGain();g.gain.value=0.2;\
    o.connect(g).connect(c.destination);o.start();\
    document.onkeypress=e=>{if(e.key==='k')r.style.background='%23ff00ff'};</script></body>";

/// The overlay browser's page: no background (transparent), an opaque red
/// block bottom-right and a 50 % green block bottom-left.
const OVERLAY: &str = "<body style='margin:0;background:transparent'>\
    <div style='position:fixed;right:0;bottom:0;width:50%;height:50%;background:%23ff0000'></div>\
    <div style='position:fixed;left:0;bottom:0;width:50%;height:50%;background:rgba(0,255,0,0.5)'></div>\
    </body>";

/// Transparent where nothing is drawn, opaque under the red block, and
/// premultiplied (about half green, alpha half) under the 50 % block.
fn overlay_pixels_ok(f: &Frame) -> Result<(), String> {
    let (top, red, half) = (f.pixel(100, 60), f.pixel(540, 300), f.pixel(100, 300));
    let premul_half = (100..=155).contains(&half[1]) && (100..=155).contains(&half[3]);
    if top[3] == 0 && close(red, RED) && premul_half && half[0] < 8 && half[2] < 8 {
        Ok(())
    } else {
        Err(format!(
            "overlay pixels: top {top:?} (want alpha 0), red {red:?}, half {half:?} (want ~[0,128,0,128])"
        ))
    }
}

fn overlay_is_transparent(slot: &FrameSlot) -> Result<(), String> {
    let start = Instant::now();
    let mut last = String::from("no overlay frame");
    while start.elapsed() < Duration::from_secs(10) {
        if let Some(f) = slot.latest() {
            if (f.width, f.height) == (W, H) {
                match overlay_pixels_ok(&f) {
                    Ok(()) => {
                        println!("test overlay_browser_is_transparent ... ok");
                        return Ok(());
                    }
                    Err(e) => last = e,
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(last)
}

/// The page renders at a new view size (the quality throttle's resolution
/// lever), and back; the overlay keeps its size.
fn set_size_resizes_page(h: &BrowserHandle, slot: &FrameSlot, overlay: &FrameSlot) -> Result<(), String> {
    h.set_size(960, 540);
    wait_for_size(slot, (960, 540), "960x540 page after set_size", |f| {
        close(f.pixel(150, 270), GREEN)
    })?;
    if let Some(f) = overlay.latest() {
        if (f.width, f.height) != (W, H) {
            return Err(format!("overlay resized to {}x{}", f.width, f.height));
        }
    }
    h.set_size(W, H);
    wait_for(slot, "page back at its size", |f| close(f.pixel(100, 180), GREEN))?;
    println!("test set_size_resizes_page ... ok");
    Ok(())
}

#[cfg(windows)]
mod gpu {
    //! The GPU path: Chromium on the chosen adapter, its shared textures
    //! opened and copied by `p2c_d3d::PageInput` on the same adapter.
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    pub struct Session {
        pub gpu: p2c_d3d::Gpu,
        pub page: p2c_d3d::PageInput,
        pub ok: Arc<AtomicU64>,
        pub failed: Arc<AtomicU64>,
        pub name: String,
        pub overlay: p2c_d3d::PageInput,
        pub overlay_ok: Arc<AtomicU64>,
    }

    /// The adapter `gpu = "auto"` picks, a device and page input on it, and
    /// the `GpuPage` that feeds it. `None` without a hardware adapter.
    #[allow(clippy::type_complexity)]
    pub fn setup() -> Option<(
        Session,
        p2c_browser::GpuPage,
        Arc<dyn Fn(isize, u32, u32) -> bool + Send + Sync>,
    )> {
        let adapters = p2c_d3d::enumerate();
        let p2c_d3d::GpuChoice::Gpu(a) = p2c_d3d::choose(&adapters, &p2c_d3d::GpuSetting::Auto).0
        else {
            return None;
        };
        let gpu = p2c_d3d::Gpu::new(a.luid).expect("D3D11 device");
        let page = p2c_d3d::PageInput::new(&gpu, W, H).expect("page input");
        let overlay = p2c_d3d::PageInput::new(&gpu, W, H).expect("overlay input");
        let overlay_ok = Arc::new(AtomicU64::new(0));
        let (ov, ook) = (overlay.clone(), overlay_ok.clone());
        let on_overlay: Arc<dyn Fn(isize, u32, u32) -> bool + Send + Sync> =
            Arc::new(move |handle, _w, _h| {
                let ok = ov.on_shared_texture(handle).is_ok();
                if ok {
                    ook.fetch_add(1, Ordering::SeqCst);
                }
                ok
            });
        let ok = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(AtomicU64::new(0));
        let (p, o, f) = (page.clone(), ok.clone(), failed.clone());
        let gpu_page = p2c_browser::GpuPage {
            adapter_luid: a.luid,
            on_texture: Arc::new(move |handle, _w, _h| match p.on_shared_texture(handle) {
                Ok(()) => {
                    o.fetch_add(1, Ordering::SeqCst);
                    true
                }
                Err(e) => {
                    if f.fetch_add(1, Ordering::SeqCst) == 0 {
                        eprintln!("shared texture failed: {e:#}");
                    }
                    false
                }
            }),
        };
        Some((
            Session {
                gpu,
                page,
                ok,
                failed,
                name: a.name,
                overlay,
                overlay_ok,
            },
            gpu_page,
            on_overlay,
        ))
    }

    /// The overlay browser's shared textures keep their transparency.
    pub fn overlay_textures_are_transparent(s: &Session) -> Result<(), String> {
        let start = Instant::now();
        let mut last = String::from("no overlay texture");
        while start.elapsed() < Duration::from_secs(10) {
            if s.overlay_ok.load(Ordering::SeqCst) > 0 {
                let f = s
                    .gpu
                    .read_bgra(&s.overlay.texture())
                    .map_err(|e| format!("{e:#}"))?;
                if (f.width, f.height) == (W, H) {
                    match super::overlay_pixels_ok(&f) {
                        Ok(()) => {
                            println!("test overlay_textures_are_transparent ... ok");
                            return Ok(());
                        }
                        Err(e) => last = e,
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(last)
    }

    /// A new view size reaches our page texture (it is recreated at the new
    /// size, no device error), and back.
    pub fn set_size_on_gpu(s: &Session, h: &p2c_browser::BrowserHandle) -> Result<(), String> {
        for (w, ht, x, y) in [(960, 540, 150, 270), (W, H, 100, 180)] {
            h.set_size(w, ht);
            let start = Instant::now();
            loop {
                let f = s
                    .gpu
                    .read_bgra(&s.page.texture())
                    .map_err(|e| format!("{e:#}"))?;
                if (f.width, f.height) == (w, ht) && close(f.pixel(x, y), GREEN) {
                    break;
                }
                if start.elapsed() > Duration::from_secs(10) {
                    return Err(format!(
                        "page texture {}x{} after set_size({w}, {ht})",
                        f.width, f.height
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        println!("test set_size_on_gpu ... ok");
        Ok(())
    }

    pub fn shared_textures_arrive(s: &Session) -> Result<(), String> {
        // Chromium's GPU process may take a few seconds to start on a dGPU.
        let start = Instant::now();
        while s.ok.load(Ordering::SeqCst) == 0 {
            if start.elapsed() > Duration::from_secs(10) {
                return Err(format!(
                    "no shared texture opened on {} within 10 s ({} failed opens)",
                    s.name,
                    s.failed.load(Ordering::SeqCst)
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let first = s.ok.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_secs(2));
        let n = s.ok.load(Ordering::SeqCst) - first;
        if n < 10 {
            return Err(format!("only {n} shared textures in 2 s"));
        }
        // Our copy holds the page (left half green).
        let f = s
            .gpu
            .read_bgra(&s.page.texture())
            .map_err(|e| format!("{e:#}"))?;
        if (f.width, f.height) != (W, H) || !close(f.pixel(100, 180), GREEN) {
            return Err(format!(
                "page texture {}x{} pixel {:?}",
                f.width,
                f.height,
                f.pixel(100, 180)
            ));
        }
        println!("  ({n} textures in 2 s on {})", s.name);
        println!("test shared_textures_arrive ... ok");
        Ok(())
    }

    /// A static page stops painting; `repaint` makes it paint again (the
    /// app needs that after moving to the adapter Chromium renders on).
    pub fn repaint_paints_a_static_page(
        s: &Session,
        h: &p2c_browser::BrowserHandle,
    ) -> Result<(), String> {
        h.navigate("data:text/html,<body style='margin:0;background:%2300f'></body>");
        // Quiet: no texture for 700 ms.
        let start = Instant::now();
        let mut last = (s.ok.load(Ordering::SeqCst), Instant::now());
        loop {
            std::thread::sleep(Duration::from_millis(20));
            let n = s.ok.load(Ordering::SeqCst);
            if n != last.0 {
                last = (n, Instant::now());
            } else if last.1.elapsed() >= Duration::from_millis(700) {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                return Err("the static page never stopped painting".into());
            }
        }
        let before = s.ok.load(Ordering::SeqCst);
        h.repaint();
        let start = Instant::now();
        while s.ok.load(Ordering::SeqCst) == before {
            if start.elapsed() > Duration::from_secs(2) {
                return Err("no texture within 2 s of repaint()".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        println!("  (painted {:?} after repaint)", start.elapsed());
        // Back to the test page (later steps recreate the browser at its URL).
        let url = format!("data:text/html,{}", super::PAGE);
        h.navigate(&url);
        let start = Instant::now();
        loop {
            let f = s
                .gpu
                .read_bgra(&s.page.texture())
                .map_err(|e| format!("{e:#}"))?;
            if close(f.pixel(100, 180), GREEN) && h.current_url() == url {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                return Err("test page did not come back after the static page".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        println!("test repaint_paints_a_static_page ... ok");
        Ok(())
    }
}

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

    // The rAF square repaints every frame: paints follow the frame rate.
    let paints_per_s = || {
        let a = slot.seq();
        std::thread::sleep(Duration::from_secs(2));
        (slot.seq() - a) as f64 / 2.0
    };
    let fast = paints_per_s();
    h.set_frame_rate(10);
    std::thread::sleep(Duration::from_millis(500));
    let slow = paints_per_s();
    h.set_frame_rate(30);
    println!("paints/s at 30 fps: {fast:.1}, at 10 fps: {slow:.1}");
    if !(fast > 20.0 && slow < 13.0) {
        return Err(format!("set_frame_rate: {fast:.1} -> {slow:.1} paints/s"));
    }
    println!("test set_frame_rate_slows_paints ... ok");

    h.set_paused(true);
    std::thread::sleep(Duration::from_millis(200));
    let paused = paints_per_s();
    h.set_paused(false);
    std::thread::sleep(Duration::from_millis(300));
    let resumed = paints_per_s();
    println!("paints/s paused: {paused:.1}, resumed: {resumed:.1}");
    if paused != 0.0 || resumed < 20.0 {
        return Err(format!(
            "set_paused: {paused:.1} then {resumed:.1} paints/s"
        ));
    }
    println!("test set_paused_drops_paints ... ok");

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
    #[cfg(windows)]
    let (session, gpu_page, on_overlay) = match gpu::setup() {
        Some((s, p, o)) => (Some(s), Some(p), Some(o)),
        None => {
            println!("no hardware GPU: shared texture step skipped");
            (None, None, None)
        }
    };
    #[cfg(not(windows))]
    let (gpu_page, on_overlay) = (None, None);
    let overlay_slot = FrameSlot::new();
    let cfg = BrowserConfig {
        url: format!("data:text/html,{PAGE}"),
        width: W,
        height: H,
        fps: 30,
        profile_dir: profile.clone(),
        audio: Some(audio.clone()),
        gpu_page,
        paused: Default::default(),
        overlay: Some(p2c_browser::OverlayBrowser {
            url: format!("data:text/html,{OVERLAY}"),
            slot: overlay_slot.clone(),
            fps: 30,
            audio: None,
            on_texture: on_overlay,
            paused: Default::default(),
        }),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let watch = slot.clone();
    p2c_browser::run(cfg, slot, move |h: BrowserHandle| {
        std::thread::spawn(move || {
            let gpu_steps = || -> Result<(), String> {
                #[cfg(windows)]
                if let Some(s) = &session {
                    gpu::shared_textures_arrive(s)?;
                    gpu::overlay_textures_are_transparent(s)?;
                    gpu::repaint_paints_a_static_page(s, &h)?;
                    gpu::set_size_on_gpu(s, &h)?;
                    // CPU fallback without re-initialising CEF: the other
                    // steps run on the recreated browser's CPU paints.
                    h.recreate_cpu();
                    wait_for(&watch, "CPU paint after recreate_cpu", |f| {
                        close(f.pixel(100, 180), GREEN)
                    })?;
                    println!("test recreate_cpu_paints_to_slot ... ok");
                }
                Ok(())
            };
            let r = gpu_steps()
                .and_then(|()| overlay_is_transparent(&overlay_slot))
                .and_then(|()| set_size_resizes_page(&h, &watch, &overlay_slot))
                .and_then(|()| scenario(&h, &watch, &audio));
            tx.send(r).unwrap();
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
