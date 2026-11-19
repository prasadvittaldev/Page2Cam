//! Real-GPU tests (Windows only, ignored by default). Each test runs on every
//! hardware adapter: `cargo test -p p2c-d3d --test gpu -- --ignored --test-threads=1`.
#![cfg(windows)]

use p2c_core::{bgra_to_nv12, camera_size, Frame, Nv12Frame};
use p2c_d3d::{
    bgra_to_nv12_gpu, enumerate, open_on_matching_adapter, AdapterInfo, Gpu, PageInput, Readback,
};

fn hardware() -> Vec<AdapterInfo> {
    let l: Vec<_> = enumerate().into_iter().filter(|a| !a.software).collect();
    assert!(!l.is_empty(), "no hardware adapter");
    l
}

fn gradient(w: u32, h: u32, salt: u32) -> Frame {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            data.push(((x * 255 / w.max(2) + salt) % 256) as u8); // B
            data.push(((y * 255 / h.max(2) + 3 * salt) % 256) as u8); // G
            data.push((((x + y) * 7 + 5 * salt) % 256) as u8); // R
            data.push(255);
        }
    }
    Frame::from_bgra(w, h, data).unwrap()
}

/// (max Y diff, max UV diff)
fn max_diff(a: &Nv12Frame, b: &Nv12Frame) -> (u8, u8) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let ylen = (a.width * a.height) as usize;
    let d = |r: std::ops::Range<usize>| {
        a.data[r.clone()]
            .iter()
            .zip(&b.data[r])
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0)
    };
    (d(0..ylen), d(ylen..a.data.len()))
}

#[test]
#[ignore]
fn device_on_each_hardware_adapter() {
    for a in hardware() {
        let gpu = Gpu::new(a.luid).unwrap_or_else(|e| panic!("{}: {e:#}", a.name));
        assert_eq!(gpu.luid(), a.luid);
        println!("device ok on {}", a.name);
    }
}

fn nv12_case(sw: u32, sh: u32, dw: u32, dh: u32, tol: u8) {
    let src = gradient(sw, sh, 0);
    let cpu = bgra_to_nv12(&src, dw, dh);
    for a in hardware() {
        let gpu = Gpu::new(a.luid).unwrap();
        let tex = gpu.upload_bgra(&src).unwrap();
        let (y, uv) = bgra_to_nv12_gpu(&gpu, &tex, dw, dh).unwrap();
        assert_eq!((y.width(), y.height()), (dw, dh));
        assert_eq!((uv.width(), uv.height()), (dw / 2, dh / 2));
        let out = gpu.read_nv12(&y, &uv).unwrap();
        let (dy, duv) = max_diff(&cpu, &out);
        println!(
            "{sw}x{sh} -> {dw}x{dh} on {}: max diff Y {dy}, UV {duv}",
            a.name
        );
        assert!(dy <= tol && duv <= tol, "{}: Y {dy} UV {duv}", a.name);
    }
}

#[test]
#[ignore]
fn nv12_gpu_matches_cpu() {
    nv12_case(64, 36, 64, 36, 2);
}

#[test]
#[ignore]
fn nv12_gpu_downscale_matches_cpu() {
    let (dw, dh) = camera_size(3840, 2160);
    nv12_case(3840, 2160, dw, dh, 3);
    // Non-integer ratio (4:3).
    let (dw, dh) = camera_size(2560, 1440);
    nv12_case(2560, 1440, dw, dh, 3);
}

#[test]
#[ignore]
fn readback_ring_lags_two_frames() {
    for a in hardware() {
        let gpu = Gpu::new(a.luid).unwrap();
        let frames: Vec<_> = (0..4).map(|i| gradient(64, 36, 40 * i + 1)).collect();
        let expect: Vec<_> = frames.iter().map(|f| bgra_to_nv12(f, 64, 36)).collect();
        let mut rb = Readback::new();
        for (i, f) in frames.iter().enumerate() {
            let tex = gpu.upload_bgra(f).unwrap();
            let (y, uv) = bgra_to_nv12_gpu(&gpu, &tex, 64, 36).unwrap();
            rb.submit(&gpu, &y, &uv).unwrap();
            let got = rb.take().unwrap();
            match i {
                0 | 1 => assert!(got.is_none(), "frame {i}: ring not yet filled"),
                _ => {
                    let got = got.expect("frame N-2");
                    let (dy, duv) = max_diff(&expect[i - 2], &got);
                    assert!(dy <= 2 && duv <= 2, "{}: got wrong frame", a.name);
                    // Distinct from the other frames.
                    let (oy, _) = max_diff(&expect[i - 1], &got);
                    assert!(oy > 10);
                    assert!(rb.take().unwrap().is_none(), "taken twice");
                }
            }
        }
        println!("readback ring ok on {}", a.name);
    }
}

#[test]
#[ignore]
fn open_page_texture_on_matching_adapter() {
    let all = hardware();
    for a in &all {
        let gpu = Gpu::new(a.luid).unwrap();
        // CEF-like: BGRA, SHARED | SHARED_NTHANDLE (0x802), NT handle.
        let shared = gpu.create_shared_bgra(&gradient(64, 36, 7)).unwrap();
        let found = open_on_matching_adapter(shared.handle(), &enumerate());
        assert_eq!(found, Some(a.luid), "{}", a.name);
        println!("shared texture from {} opened on matching adapter", a.name);
    }
}

#[test]
#[ignore]
fn page_input_copies_shared_texture() {
    let all = hardware();
    for a in &all {
        let gpu = Gpu::new(a.luid).unwrap();
        let src = gradient(64, 36, 9);
        let shared = gpu.create_shared_bgra(&src).unwrap();
        let h = shared.handle();
        let page = PageInput::new(&gpu, 64, 36).unwrap();
        assert_eq!(page.seq(), 0);
        page.on_shared_texture(h).unwrap();
        page.on_shared_texture(h).unwrap(); // cached handle
        assert_eq!(page.seq(), 2);
        assert_eq!(page.adapter_mismatch(), None);
        let back = gpu.read_bgra(&page.texture()).unwrap();
        assert_eq!(back.data, src.data, "{}", a.name);

        // A handle from another adapter: error + mismatch report.
        for b in all.iter().filter(|b| b.luid != a.luid) {
            let other = Gpu::new(b.luid).unwrap();
            let page = PageInput::new(&other, 64, 36).unwrap();
            assert!(page.on_shared_texture(h).is_err());
            assert_eq!(page.adapter_mismatch(), Some(a.luid));
            assert_eq!(page.seq(), 0);
        }
        drop(shared);
        println!("page input ok on {}", a.name);
    }
}

#[test]
#[ignore]
fn readback_ring_rebuilt_for_recreated_device_on_same_adapter() {
    for a in hardware() {
        let f = gradient(64, 36, 3);
        let old = Gpu::new(a.luid).unwrap();
        let mut rb = Readback::new();
        for _ in 0..3 {
            let tex = old.upload_bgra(&f).unwrap();
            let (y, uv) = bgra_to_nv12_gpu(&old, &tex, 64, 36).unwrap();
            rb.submit(&old, &y, &uv).unwrap();
        }
        // "Recreated after a device loss": same adapter, new device.
        let new = Gpu::new(a.luid).unwrap();
        assert!(!new.same(&old) && new.same(&new.clone()));
        let tex = new.upload_bgra(&f).unwrap();
        let (y, uv) = bgra_to_nv12_gpu(&new, &tex, 64, 36).unwrap();
        rb.submit(&new, &y, &uv).unwrap();
        // A fresh ring: nothing from the old device's staging textures.
        assert!(rb.take().unwrap().is_none(), "{}: old ring kept", a.name);
        for _ in 0..2 {
            rb.submit(&new, &y, &uv).unwrap();
        }
        let got = rb.take().unwrap().expect("ring filled on the new device");
        assert_eq!(got, bgra_to_nv12(&f, 64, 36));
        println!("readback ring rebuilt on {}", a.name);
    }
}

#[test]
#[ignore]
fn unopenable_handle_is_probed_once() {
    for a in hardware() {
        let gpu = Gpu::new(a.luid).unwrap();
        // A handle value that no adapter opens: a shared handle already closed.
        let h = gpu.create_shared_bgra(&gradient(8, 8, 1)).unwrap().handle();
        let page = PageInput::new(&gpu, 8, 8).unwrap();
        for _ in 0..5 {
            assert!(page.on_shared_texture(h).is_err());
        }
        assert_eq!(page.probe_count(), 1, "{}", a.name);
        assert_eq!(page.adapter_mismatch(), None);
        assert_eq!(page.seq(), 0);
        println!("probed once on {}", a.name);
    }
}

// ---------------------------------------------------------------------------
// GPU compositor (Task 7)

use std::sync::Arc;
use std::time::{Duration, Instant};

use p2c_compositor::{
    Compositor, OutputFormat, OutputProducer, OutputSpec, OverlayStyle, Pip, PipLayout, TickInputs,
};
use p2c_core::{EdgeRange, Mask};
use p2c_d3d::GpuProducer;

/// Deterministic per-pixel noise (worst case for any filter mismatch).
fn noise(w: u32, h: u32, salt: u32) -> Frame {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    let mut s = salt.wrapping_mul(2654435761).wrapping_add(12345);
    for _ in 0..w * h {
        for _ in 0..3 {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            data.push((s >> 8) as u8);
        }
        data.push(255);
    }
    Frame::from_bgra(w, h, data).unwrap()
}

/// A smooth person-like mask: a ramp from left to right plus a blob.
fn soft_mask(w: u32, h: u32, edge: EdgeRange) -> Arc<Mask> {
    let mut data = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 / w as f32 - 0.5;
            let dy = y as f32 / h as f32 - 0.6;
            let blob = (1.0 - (dx * dx + dy * dy).sqrt() * 3.0).clamp(0.0, 1.0);
            let ramp = x as f32 / (w - 1) as f32;
            data.push(((0.5 * ramp + 0.5 * blob) * 255.0).round() as u8);
        }
    }
    let mut m = Mask::new(w, h, data).unwrap();
    m.edge = edge;
    Arc::new(m)
}

fn spec(w: u32, h: u32) -> OutputSpec {
    OutputSpec {
        width: w,
        height: h,
        fps: 30,
        format: OutputFormat::Nv12,
    }
}

/// A producer whose page input already holds `page` (via a CEF-like shared texture).
fn producer_with_page(gpu: &Gpu, page: &Frame, spec: OutputSpec) -> GpuProducer {
    let input = PageInput::new(gpu, page.width, page.height).unwrap();
    let shared = gpu.create_shared_bgra(page).unwrap();
    input.on_shared_texture(shared.handle()).unwrap();
    GpuProducer::new(gpu.clone(), input, spec).unwrap()
}

/// Largest per-channel difference and where it is.
fn bgra_diff(a: &Frame, b: &Frame) -> (u8, u32, u32) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let mut worst = (0u8, 0, 0);
    for (i, (x, y)) in a.data.iter().zip(&b.data).enumerate() {
        let d = x.abs_diff(*y);
        if d > worst.0 {
            let px = (i / 4) as u32;
            worst = (d, px % a.width, px / a.width);
        }
    }
    worst
}

/// GPU BGRA intermediate vs the CPU `Compositor` on identical inputs (±2).
fn parity_case(
    label: &str,
    page: &Frame,
    (ow, oh): (u32, u32),
    cam: Option<Arc<Frame>>,
    mask: Option<Arc<Mask>>,
    layout: PipLayout,
) {
    let page_arc = Arc::new(page.clone());
    let mut cpu = Frame::new_black(ow, oh);
    let pip = cam.as_ref().map(|cam| Pip {
        cam,
        mask: mask.as_ref(),
        layout: &layout,
    });
    Compositor::new().compose(Some(&page_arc), pip, &mut cpu);
    for a in hardware() {
        let gpu = Gpu::new(a.luid).unwrap();
        let mut p = producer_with_page(&gpu, page, spec(ow, oh));
        let inputs = TickInputs {
            page: None,
            cam: cam.clone(),
            mask: mask.clone(),
            layout,
        };
        let got = p.render_bgra_for_test(&inputs).unwrap();
        let (d, x, y) = bgra_diff(&cpu, &got);
        println!("{label} on {}: max diff {d} at ({x},{y})", a.name);
        assert!(
            d <= 2,
            "{label} on {}: diff {d} at ({x},{y}): cpu {:?} gpu {:?}",
            a.name,
            cpu.pixel(x, y),
            got.pixel(x, y)
        );
    }
}

fn layout(style: OverlayStyle, x: f32, y: f32, width: f32) -> PipLayout {
    PipLayout {
        visible: true,
        x,
        y,
        width,
        style,
    }
}

#[test]
#[ignore]
fn parity_box() {
    parity_case(
        "box",
        &noise(640, 360, 1),
        (640, 360),
        Some(Arc::new(noise(320, 240, 2))),
        None,
        layout(OverlayStyle::Box, 0.3, 0.6, 0.3),
    );
}

#[test]
#[ignore]
fn parity_cutout_small() {
    parity_case(
        "cutout small",
        &noise(640, 360, 3),
        (640, 360),
        Some(Arc::new(noise(320, 180, 4))),
        Some(soft_mask(80, 45, EdgeRange::FULL)),
        layout(OverlayStyle::CutoutSmall, 0.7, 0.0, 0.4),
    );
}

#[test]
#[ignore]
fn parity_cutout_full_overhang_left() {
    parity_case(
        "cutout full x=0",
        &noise(640, 360, 5),
        (640, 360),
        Some(Arc::new(noise(320, 180, 6))),
        Some(soft_mask(160, 90, EdgeRange::FULL)),
        layout(OverlayStyle::CutoutFull, 0.0, 0.0, 0.25),
    );
}

#[test]
#[ignore]
fn parity_mask_edge_range() {
    let mut m = Mask::new(40, 30, vec![100; 40 * 30]).unwrap();
    m.edge = EdgeRange { lo: 64, hi: 192 };
    parity_case(
        "edge range, mask 100",
        &noise(640, 360, 7),
        (640, 360),
        Some(Arc::new(noise(320, 240, 8))),
        Some(Arc::new(m)),
        layout(OverlayStyle::CutoutFull, 0.5, 0.0, 0.25),
    );
    // And a soft mask through the same edge range.
    parity_case(
        "edge range, soft mask",
        &noise(640, 360, 9),
        (640, 360),
        Some(Arc::new(noise(320, 240, 10))),
        Some(soft_mask(64, 48, EdgeRange { lo: 64, hi: 192 })),
        layout(OverlayStyle::CutoutSmall, 0.2, 0.0, 0.5),
    );
}

#[test]
#[ignore]
fn parity_stale_mask_is_box() {
    parity_case(
        "cut-out without mask",
        &noise(640, 360, 11),
        (640, 360),
        Some(Arc::new(noise(320, 180, 12))),
        None,
        layout(OverlayStyle::CutoutFull, 0.2, 0.5, 0.3),
    );
}

#[test]
#[ignore]
fn parity_letterbox_4_3_page() {
    // 4:3 page into 16:9 (pillarbox, upscaled) and a page larger than the output.
    parity_case(
        "4:3 page",
        &noise(400, 300, 13),
        (640, 360),
        Some(Arc::new(noise(160, 120, 14))),
        None,
        layout(OverlayStyle::Box, 1.0, 1.0, 0.25),
    );
    parity_case(
        "4:3 page, downscaled",
        &noise(1024, 768, 15),
        (640, 360),
        None,
        None,
        PipLayout::default(),
    );
}

#[test]
#[ignore]
fn gpu_no_page_yet_is_black_with_webcam() {
    for a in hardware() {
        let gpu = Gpu::new(a.luid).unwrap();
        let input = PageInput::new(&gpu, 640, 360).unwrap();
        let mut p = GpuProducer::new(gpu.clone(), input, spec(640, 360)).unwrap();
        let cam = Arc::new(noise(320, 180, 16));
        let l = layout(OverlayStyle::Box, 0.0, 0.0, 0.25);
        let got = p
            .render_bgra_for_test(&TickInputs {
                page: None,
                cam: Some(cam.clone()),
                mask: None,
                layout: l,
            })
            .unwrap();
        let mut cpu = Frame::new_black(640, 360);
        let pip = Pip {
            cam: &cam,
            mask: None,
            layout: &l,
        };
        Compositor::new().compose(None, Some(pip), &mut cpu);
        let (d, x, y) = bgra_diff(&cpu, &got);
        assert!(d <= 2, "{}: diff {d} at ({x},{y})", a.name);
        println!("no page yet on {}: max diff {d}", a.name);
    }
}

#[test]
#[ignore]
fn gpu_output_4k_camera_downscaled() {
    let page = gradient(3840, 2160, 17);
    for a in hardware() {
        let gpu = Gpu::new(a.luid).unwrap();
        let mut p = producer_with_page(&gpu, &page, spec(3840, 2160));
        let inputs = TickInputs {
            page: None,
            cam: Some(Arc::new(gradient(1280, 720, 18))),
            mask: None,
            layout: PipLayout::default(),
        };
        // The readback ring lags two frames; unchanged inputs drain it.
        let mut out = None;
        for _ in 0..3 {
            if let Some(o) = p.produce(&inputs, false).unwrap() {
                out = Some(o);
            }
        }
        let out = out.expect("an output after three ticks");
        let nv12 = out.nv12.expect("nv12");
        let camera = out.camera.expect("camera");
        assert_eq!((nv12.width, nv12.height), (3840, 2160));
        assert_eq!((camera.width, camera.height), (1920, 1080));
        assert!(!Arc::ptr_eq(&nv12, &camera));
        let bgra = p.render_bgra_for_test(&inputs).unwrap();
        let (dy, duv) = max_diff(&bgra_to_nv12(&bgra, 3840, 2160), &nv12);
        let (cy, cuv) = max_diff(&bgra_to_nv12(&bgra, 1920, 1080), &camera);
        println!(
            "4K on {}: nv12 diff Y {dy} UV {duv}, camera diff Y {cy} UV {cuv}",
            a.name
        );
        assert!(dy <= 2 && duv <= 2, "{}: nv12 Y {dy} UV {duv}", a.name);
        assert!(cy <= 3 && cuv <= 3, "{}: camera Y {cy} UV {cuv}", a.name);
    }
}

#[test]
#[ignore]
fn gpu_producer_skips_unchanged_and_drains() {
    for a in hardware() {
        let gpu = Gpu::new(a.luid).unwrap();
        let mut p = producer_with_page(&gpu, &gradient(640, 360, 19), spec(640, 360));
        let inputs = TickInputs {
            page: None,
            cam: Some(Arc::new(gradient(320, 180, 20))),
            mask: None,
            layout: PipLayout::default(),
        };
        let outs: Vec<_> = (0..6).map(|_| p.produce(&inputs, false).unwrap()).collect();
        // Tick 0 renders (ring not filled yet), ticks 1-2 drain it, then nothing.
        let some: Vec<bool> = outs.iter().map(|o| o.is_some()).collect();
        assert_eq!(
            some,
            [false, false, true, false, false, false],
            "{}",
            a.name
        );
        let last = outs[2].as_ref().unwrap();
        let nv12 = last.nv12.as_ref().unwrap();
        // Same size: the camera frame is the same Arc.
        assert!(Arc::ptr_eq(nv12, last.camera.as_ref().unwrap()));
        assert!(last.preview.is_none());
        let bgra = p.render_bgra_for_test(&inputs).unwrap();
        let (dy, duv) = max_diff(&bgra_to_nv12(&bgra, 640, 360), nv12);
        assert!(dy <= 2 && duv <= 2, "{}: Y {dy} UV {duv}", a.name);

        // A due preview for a picture never previewed: an output with a preview.
        let o = p.produce(&inputs, true).unwrap();
        let o2 = p.produce(&inputs, true).unwrap();
        let preview = o
            .iter()
            .chain(o2.iter())
            .find_map(|o| o.preview.clone())
            .expect("preview within two due ticks");
        assert_eq!((preview.width, preview.height), (640, 360));
        assert!(p.produce(&inputs, true).unwrap().is_none(), "nothing left");
        println!("skip/drain/preview ok on {}", a.name);
    }
}

#[test]
#[ignore]
fn bench_compose_1080p_cutout_full() {
    let page = noise(1920, 1080, 21);
    let cams: Vec<_> = (0..4).map(|i| Arc::new(noise(1280, 720, 22 + i))).collect();
    let masks: Vec<_> = (0..4)
        .map(|i| {
            let edge = if i % 2 == 0 {
                EdgeRange::FULL
            } else {
                EdgeRange { lo: 40, hi: 220 }
            };
            soft_mask(640, 360, edge)
        })
        .collect();
    for a in hardware() {
        let gpu = Gpu::new(a.luid).unwrap();
        let input = PageInput::new(&gpu, 1920, 1080).unwrap();
        let shared = gpu.create_shared_bgra(&page).unwrap();
        let mut p = GpuProducer::new(gpu.clone(), input.clone(), spec(1920, 1080)).unwrap();
        let l = layout(OverlayStyle::CutoutFull, 0.5, 0.0, 0.25);
        let mut times = Vec::new();
        for i in 0..230usize {
            // Every input changes every frame (CEF copies on its own thread).
            input.on_shared_texture(shared.handle()).unwrap();
            let inputs = TickInputs {
                page: None,
                cam: Some(Arc::new((*cams[i % 4]).clone())),
                mask: Some(masks[i % 4].clone()),
                layout: l,
            };
            let t = Instant::now();
            let out = p.produce(&inputs, false).unwrap();
            let dt = t.elapsed();
            if i >= 2 {
                assert!(out.is_some_and(|o| o.nv12.is_some()));
            }
            if i >= 30 {
                times.push(dt);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        times.sort();
        let pct = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
        println!(
            "bench 1080p cutout-full on {}: p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms ({} frames)",
            a.name,
            pct(0.5).as_secs_f64() * 1e3,
            pct(0.95).as_secs_f64() * 1e3,
            pct(1.0).as_secs_f64() * 1e3,
            times.len()
        );
    }
}

// ---- Device-loss recovery (Task 8) ----

use p2c_d3d::{is_device_lost, PageHolder, RecoveringProducer};

/// Sets `P2C_FAKE_DEVICE_LOSS` for the guard's lifetime (tests run with
/// `--test-threads=1`, so no other producer sees it).
struct FakeLoss;
impl FakeLoss {
    fn after(n: u32) -> FakeLoss {
        std::env::set_var("P2C_FAKE_DEVICE_LOSS", n.to_string());
        FakeLoss
    }
}
impl Drop for FakeLoss {
    fn drop(&mut self) {
        std::env::remove_var("P2C_FAKE_DEVICE_LOSS");
    }
}

fn no_inputs() -> TickInputs {
    TickInputs {
        page: None,
        cam: None,
        mask: None,
        layout: PipLayout::default(),
    }
}

#[test]
#[ignore]
fn device_loss_recreates_once_then_fails() {
    if !cfg!(debug_assertions) {
        println!("P2C_FAKE_DEVICE_LOSS is debug-only; skipped");
        return;
    }
    for a in hardware() {
        let _fake = FakeLoss::after(5);
        let holder = PageHolder::default();
        let mut p = RecoveringProducer::new(a.luid, spec(64, 36), holder.clone()).unwrap();
        let first_gpu = p.gpu().clone();
        let first_page = holder.current().expect("page input published");
        let inputs = no_inputs();
        for i in 1..=5 {
            p.produce(&inputs, false)
                .unwrap_or_else(|e| panic!("call {i}: {e:#}"));
        }
        // Call 6: device lost -> error returned, rebuild started.
        let e = p.produce(&inputs, false).unwrap_err();
        assert!(is_device_lost(&e), "{e:#}");
        // Until the new device is ready: Ok(None) (the ticker repeats).
        let start = Instant::now();
        let mut waits = 0;
        let mut calls_on_new;
        let mut got_frame = false;
        loop {
            assert!(start.elapsed() < Duration::from_secs(5), "rebuild hangs");
            let out = p
                .produce(&inputs, false)
                .unwrap_or_else(|e| panic!("{e:#}"));
            if !p.gpu().same(&first_gpu) {
                calls_on_new = 1;
                got_frame |= out.is_some();
                break;
            }
            assert!(out.is_none());
            waits += 1;
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(p.gpu().luid(), a.luid);
        // Shared textures now reach the new device's page input.
        let shared = p.gpu().create_shared_bgra(&gradient(64, 36, 1)).unwrap();
        holder.on_shared_texture(shared.handle()).unwrap();
        assert_eq!(holder.current().unwrap().seq(), 1);
        assert_eq!(first_page.seq(), 0, "old page input still fed");
        // The new producer works (its own 5 calls), then loses the device
        // again: no second recreate.
        let second_gpu = p.gpu().clone();
        while calls_on_new < 5 {
            calls_on_new += 1;
            let out = p
                .produce(&inputs, false)
                .unwrap_or_else(|e| panic!("after recreate, call {calls_on_new}: {e:#}"));
            got_frame |= out.is_some_and(|o| o.nv12.is_some());
        }
        assert!(got_frame, "recreated producer gave no frame");
        println!(
            "  rebuild took {:?} ({waits} waiting calls)",
            start.elapsed()
        );
        for _ in 0..3 {
            let e = p.produce(&inputs, false).unwrap_err();
            assert!(is_device_lost(&e), "{e:#}");
            assert!(p.gpu().same(&second_gpu), "recreated twice");
        }
        println!("recreated once, then failed, on {}", a.name);
    }
}

#[test]
#[ignore]
fn ticker_falls_back_to_cpu_after_second_device_loss() {
    use p2c_compositor::{FrameSink, Output, ProducerEvent, Ticker};
    use p2c_core::FrameSlot;
    if !cfg!(debug_assertions) {
        println!("P2C_FAKE_DEVICE_LOSS is debug-only; skipped");
        return;
    }
    struct Rec(Arc<std::sync::Mutex<Vec<(Instant, Output)>>>);
    impl FrameSink for Rec {
        fn push(&mut self, out: &Output) -> anyhow::Result<()> {
            self.0.lock().unwrap().push((Instant::now(), out.clone()));
            Ok(())
        }
    }
    let a = hardware().into_iter().min_by_key(|a| a.perf_rank).unwrap();
    // Kept for the whole test: the recreated producer reads it too.
    let _fake = FakeLoss::after(10);
    let s = spec(64, 36);
    let p = RecoveringProducer::new(a.luid, s, PageHolder::default()).unwrap();
    // A CPU page for the fallback (the browser would repaint on the CPU).
    let page = FrameSlot::new();
    page.put(gradient(64, 36, 2));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (tx, rx) = std::sync::mpsc::channel();
    let t = Ticker::spawn(
        s,
        Some(page),
        None,
        None,
        Arc::new(parking_lot::RwLock::new(PipLayout::default())),
        Box::new(p),
        vec![Box::new(Rec(seen.clone()))],
        tx,
    );
    let ev = rx.recv_timeout(Duration::from_secs(5));
    let at = Instant::now();
    std::thread::sleep(Duration::from_millis(300));
    t.stop();
    let ProducerEvent::Failed(reason) = ev.expect("no fallback event");
    assert!(reason.contains("fake device loss"), "{reason}");
    let seen = seen.lock().unwrap();
    let gaps = seen
        .windows(2)
        .map(|w| w[1].0.duration_since(w[0].0))
        .max()
        .unwrap();
    assert!(gaps < Duration::from_millis(100), "sinks stalled {gaps:?}");
    let after: Vec<_> = seen.iter().filter(|(t, _)| *t > at).collect();
    assert!(
        after.len() >= 5,
        "{} pushes after the fallback",
        after.len()
    );
    let cpu = bgra_to_nv12(&gradient(64, 36, 2), 64, 36);
    let last = after.last().unwrap().1.nv12.clone().expect("nv12");
    assert_eq!(last.data, cpu.data, "not the CPU compositor's frame");
    println!("fell back on {}: {reason}", a.name);
}

#[test]
#[ignore]
fn hung_rebuild_errors_within_a_second_and_ticker_falls_back() {
    use p2c_compositor::{FrameSink, Output, ProducerEvent, Ticker};
    if !cfg!(debug_assertions) {
        println!("P2C_FAKE_DEVICE_LOSS is debug-only; skipped");
        return;
    }
    let a = hardware().into_iter().min_by_key(|a| a.perf_rank).unwrap();
    let _fake = FakeLoss::after(5);
    let inputs = no_inputs();

    // The producer alone: after the loss, a rebuild that hangs (here 5 s)
    // becomes an error after ~1 s, and stays one.
    let mut p = RecoveringProducer::new(a.luid, spec(64, 36), PageHolder::default()).unwrap();
    p.delay_rebuild_for_test(Duration::from_secs(5));
    for _ in 0..5 {
        p.produce(&inputs, false).unwrap();
    }
    assert!(
        p.produce(&inputs, false).is_err(),
        "device loss not reported"
    );
    let start = Instant::now();
    let e = loop {
        match p.produce(&inputs, false) {
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Ok(Some(_)) => panic!("frame from a hung rebuild"),
            Err(e) => break e,
        }
        assert!(start.elapsed() < Duration::from_secs(3), "no deadline");
    };
    let waited = start.elapsed();
    println!("  hung rebuild reported after {waited:?}: {e:#}");
    assert!(waited <= Duration::from_millis(1200), "{waited:?}");
    assert!(p.produce(&inputs, false).is_err(), "error not kept");
    // The ticker's wait for the CPU page counts from the start of the hold,
    // so the two waits don't add up (M1).
    let held = p.holding_since().expect("hold start kept after giving up");
    assert!(held <= start && start - held < Duration::from_millis(100));

    // In the ticker: Failed, and frames keep flowing.
    struct Count(Arc<std::sync::atomic::AtomicUsize>);
    impl FrameSink for Count {
        fn push(&mut self, _: &Output) -> anyhow::Result<()> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }
    let mut p = RecoveringProducer::new(a.luid, spec(64, 36), PageHolder::default()).unwrap();
    p.delay_rebuild_for_test(Duration::from_secs(5));
    let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (tx, rx) = std::sync::mpsc::channel();
    let t = Ticker::spawn(
        spec(64, 36),
        Some(p2c_core::FrameSlot::new()),
        None,
        None,
        Arc::new(parking_lot::RwLock::new(PipLayout::default())),
        Box::new(p),
        vec![Box::new(Count(n.clone()))],
        tx,
    );
    let ev = rx.recv_timeout(Duration::from_secs(3));
    let before = n.load(std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(300));
    t.stop();
    let ProducerEvent::Failed(reason) = ev.expect("ticker never fell back");
    println!("  ticker fell back: {reason}");
    assert!(n.load(std::sync::atomic::Ordering::SeqCst) >= before + 5);
}

// ---- Chromium on another adapter (Review Focus #3) ----

#[test]
#[ignore]
fn producer_moves_to_the_adapter_chromium_renders_on() {
    let all = hardware();
    if all.len() < 2 {
        println!("needs two hardware adapters; skipped");
        return;
    }
    for (a, b) in [(&all[0], &all[1]), (&all[1], &all[0])] {
        // We start on A; Chromium ignored the adapter switch and renders on B.
        let holder = PageHolder::default();
        let mut p = RecoveringProducer::new(a.luid, spec(64, 36), holder.clone()).unwrap();
        let moved = Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let m = moved.clone();
            p.on_adapter_change(Box::new(move |ad: &AdapterInfo| {
                m.lock().unwrap().push(ad.name.clone())
            }));
        }
        let chromium = Gpu::new(b.luid).unwrap();
        let src = gradient(64, 36, 5);
        let shared = chromium.create_shared_bgra(&src).unwrap();
        let e = holder.on_shared_texture(shared.handle()).unwrap_err();
        let msg = format!("{e:#}");
        // The reason names both adapters, not raw LUIDs.
        assert!(msg.contains(&b.name) && msg.contains(&a.name), "{msg}");
        let start = Instant::now();
        let mut frames = 0;
        while p.gpu().luid() != b.luid {
            assert!(start.elapsed() < Duration::from_secs(10), "never moved");
            // Every call succeeds: the ticker never falls back to the CPU.
            if p.produce(&no_inputs(), false)
                .unwrap_or_else(|e| panic!("{} -> {}: {e:#}", a.name, b.name))
                .is_some()
            {
                frames += 1;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        println!(
            "  moved {} -> {} in {:?} ({frames} frames meanwhile)",
            a.name,
            b.name,
            start.elapsed()
        );
        // Chromium's textures now open on our device.
        holder.on_shared_texture(shared.handle()).unwrap();
        let page = holder.current().unwrap();
        assert_eq!(page.seq(), 1);
        assert_eq!(page.adapter_mismatch(), None);
        assert_eq!(*moved.lock().unwrap(), vec![b.name.clone()]);
        let back = p.gpu().read_bgra(&page.texture()).unwrap();
        assert_eq!(back.data, src.data);
        for _ in 0..5 {
            p.produce(&no_inputs(), false).unwrap();
        }
    }
}
