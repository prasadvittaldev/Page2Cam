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
        page.on_shared_texture(h).unwrap(); // same handle again
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

fn solid(w: u32, h: u32, bgra: [u8; 4]) -> Frame {
    Frame::from_bgra(w, h, bgra.repeat((w * h) as usize)).unwrap()
}

/// CEF's NT handle is valid only during `on_accelerated_paint`; once closed,
/// Windows hands the same value out again. A new texture behind a recycled
/// handle value must show up, not a texture from Chromium's previous pool
/// (seen as old/new page flicker after a navigation).
#[test]
#[ignore]
fn recycled_handle_value_shows_the_new_texture() {
    const RED: [u8; 4] = [0, 0, 255, 255];
    const BLUE: [u8; 4] = [255, 0, 0, 255];
    for a in hardware() {
        let gpu = Gpu::new(a.luid).unwrap();
        let page = PageInput::new(&gpu, 32, 16).unwrap();
        let tex_a = gpu.create_shared_bgra(&solid(32, 16, RED)).unwrap();
        let ha = tex_a.handle();
        page.on_shared_texture(ha).unwrap();
        assert_eq!(
            gpu.read_bgra(&page.texture()).unwrap().data,
            solid(32, 16, RED).data
        );
        // Close A's handle and release A (Chromium rebuilding its pool).
        drop(tex_a);
        // Windows hands freed handle values out again; create (and close)
        // textures until one gets A's old value.
        let mut tries = 0;
        let mut seen = Vec::new();
        let tex_b = loop {
            let b = gpu.create_shared_bgra(&solid(32, 16, BLUE)).unwrap();
            if b.handle() == ha {
                break b;
            }
            tries += 1;
            if seen.len() < 16 {
                seen.push(b.handle());
            }
            assert!(
                tries < 10_000,
                "{}: handle value {ha:#x} never reused, got {seen:x?}...",
                a.name
            );
        };
        println!(
            "{}: handle value {ha:#x} reused after {tries} other handles",
            a.name
        );
        page.on_shared_texture(tex_b.handle()).unwrap();
        let back = gpu.read_bgra(&page.texture()).unwrap();
        assert_eq!(
            back.pixel(0, 0),
            solid(1, 1, BLUE).pixel(0, 0),
            "{}: stale texture behind a recycled handle value",
            a.name
        );
        assert_eq!(back.data, solid(32, 16, BLUE).data, "{}", a.name);

        // Per-callback cost (open + copy + flush) at 1080p.
        let big = gpu.create_shared_bgra(&gradient(1920, 1080, 3)).unwrap();
        let page = PageInput::new(&gpu, 1920, 1080).unwrap();
        page.on_shared_texture(big.handle()).unwrap();
        let mut times: Vec<_> = (0..200)
            .map(|_| {
                let t = Instant::now();
                page.on_shared_texture(big.handle()).unwrap();
                t.elapsed()
            })
            .collect();
        times.sort();
        println!(
            "  on_shared_texture 1080p on {}: p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms",
            a.name,
            times[100].as_secs_f64() * 1e3,
            times[190].as_secs_f64() * 1e3,
            times[199].as_secs_f64() * 1e3
        );
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

/// Mean absolute difference per channel (B, G, R, A) over the whole frame.
fn bgra_mean_diff(a: &Frame, b: &Frame) -> [f64; 4] {
    let mut sum = [0u64; 4];
    for (i, (x, y)) in a.data.iter().zip(&b.data).enumerate() {
        sum[i % 4] += u64::from(x.abs_diff(*y));
    }
    let n = (a.width as u64 * a.height as u64).max(1) as f64;
    sum.map(|s| s as f64 / n)
}

/// Allowed GPU-vs-CPU difference: max per channel, and mean per channel.
#[derive(Clone, Copy)]
struct Tol {
    max: u8,
    mean: f64,
}

/// Unscaled, letterbox, Box geometry, edge remap: the exact filter (±2).
const STRICT: Tol = Tol { max: 2, mean: 1.0 };
/// Upscaled webcam/mask in the cached PIP layer use the hardware bilinear
/// sampler (R41), which differs from fast_image_resize's byte-rounded
/// two-pass convolution by a little on noise, more where a steep edge
/// remap amplifies a mask difference.
const SCALED_PIP: Tol = Tol { max: 8, mean: 1.0 };

/// GPU BGRA intermediate vs the CPU `Compositor` on identical inputs (±2).
fn parity_case(
    label: &str,
    page: &Frame,
    out: (u32, u32),
    cam: Option<Arc<Frame>>,
    mask: Option<Arc<Mask>>,
    layout: PipLayout,
) {
    parity_case_tol(label, page, out, cam, mask, layout, STRICT)
}

fn parity_case_tol(
    label: &str,
    page: &Frame,
    (ow, oh): (u32, u32),
    cam: Option<Arc<Frame>>,
    mask: Option<Arc<Mask>>,
    layout: PipLayout,
    tol: Tol,
) {
    let page_arc = Arc::new(page.clone());
    let mut cpu = Frame::new_black(ow, oh);
    let pip = cam.as_ref().map(|cam| Pip {
        cam,
        mask: mask.as_ref(),
        layout: &layout,
    });
    Compositor::new().compose(
        Some(&page_arc),
        pip,
        &p2c_compositor::Backdrop::default(),
        &mut cpu,
    );
    for a in hardware() {
        let gpu = Gpu::new(a.luid).unwrap();
        let mut p = producer_with_page(&gpu, page, spec(ow, oh));
        let inputs = TickInputs {
            page: None,
            cam: cam.clone(),
            mask: mask.clone(),
            layout,
            backdrop: p2c_compositor::Backdrop::default(),
        };
        let got = p.render_bgra_for_test(&inputs).unwrap();
        let (d, x, y) = bgra_diff(&cpu, &got);
        let mean = bgra_mean_diff(&cpu, &got);
        println!(
            "{label} on {}: max diff {d} at ({x},{y}), mean {:.3?}",
            a.name, mean
        );
        assert!(
            d <= tol.max,
            "{label} on {}: diff {d} > {} at ({x},{y}): cpu {:?} gpu {:?}",
            a.name,
            tol.max,
            cpu.pixel(x, y),
            got.pixel(x, y)
        );
        assert!(
            mean.iter().all(|m| *m <= tol.mean),
            "{label} on {}: mean diff {mean:?} > {}",
            a.name,
            tol.mean
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
    parity_case_tol(
        "cutout small",
        &noise(640, 360, 3),
        (640, 360),
        Some(Arc::new(noise(320, 180, 4))),
        Some(soft_mask(80, 45, EdgeRange::FULL)),
        layout(OverlayStyle::CutoutSmall, 0.7, 0.0, 0.4),
        SCALED_PIP,
    );
}

#[test]
#[ignore]
fn parity_cutout_full_overhang_left() {
    parity_case_tol(
        "cutout full x=0",
        &noise(640, 360, 5),
        (640, 360),
        Some(Arc::new(noise(320, 180, 6))),
        Some(soft_mask(160, 90, EdgeRange::FULL)),
        layout(OverlayStyle::CutoutFull, 0.0, 0.0, 0.25),
        SCALED_PIP,
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
    parity_case_tol(
        "edge range, soft mask",
        &noise(640, 360, 9),
        (640, 360),
        Some(Arc::new(noise(320, 240, 10))),
        Some(soft_mask(64, 48, EdgeRange { lo: 64, hi: 192 })),
        layout(OverlayStyle::CutoutSmall, 0.2, 0.0, 0.5),
        SCALED_PIP,
    );
}

/// The live case: 1080p output, 1280x720 webcam and 640x360 mask, both
/// upscaled into the cut-out-full rectangle.
#[test]
#[ignore]
fn parity_cutout_full_1080p_live_sizes() {
    parity_case_tol(
        "cutout full 1080p",
        &noise(1920, 1080, 41),
        (1920, 1080),
        Some(Arc::new(noise(1280, 720, 42))),
        Some(soft_mask(640, 360, EdgeRange { lo: 40, hi: 220 })),
        layout(OverlayStyle::CutoutFull, 0.5, 0.0, 0.25),
        SCALED_PIP,
    );
}

/// R41: the scaled webcam x mask layer is built only when the webcam, mask,
/// style or PIP size changes; page-only changes and moves reuse it.
#[test]
#[ignore]
fn gpu_pip_layer_rebuilt_only_on_change() {
    let page = noise(640, 360, 43);
    for a in hardware() {
        let gpu = Gpu::new(a.luid).unwrap();
        let input = PageInput::new(&gpu, 640, 360).unwrap();
        let shared = gpu.create_shared_bgra(&page).unwrap();
        let mut p = GpuProducer::new(gpu.clone(), input.clone(), spec(640, 360)).unwrap();
        let cam = Arc::new(noise(320, 180, 44));
        let mask = soft_mask(160, 90, EdgeRange { lo: 40, hi: 220 });
        let mut inputs = TickInputs {
            page: None,
            cam: Some(cam.clone()),
            mask: Some(mask.clone()),
            layout: layout(OverlayStyle::CutoutSmall, 0.3, 0.0, 0.4),
            backdrop: p2c_compositor::Backdrop::default(),
        };
        // The page changes every tick (animated page): one layer build.
        for _ in 0..5 {
            input.on_shared_texture(shared.handle()).unwrap();
            p.produce(&inputs, false).unwrap();
        }
        assert_eq!(p.layer_builds(), 1, "{}: page-only changes", a.name);
        // Moving the PIP (same size) reuses the layer.
        inputs.layout.x = 0.8;
        p.produce(&inputs, false).unwrap();
        assert_eq!(p.layer_builds(), 1, "{}: move", a.name);
        // A new mask, a new webcam frame, a new size, a new style: rebuilt.
        inputs.mask = Some(soft_mask(160, 90, EdgeRange { lo: 40, hi: 220 }));
        p.produce(&inputs, false).unwrap();
        assert_eq!(p.layer_builds(), 2, "{}: new mask", a.name);
        inputs.cam = Some(Arc::new((*cam).clone()));
        p.produce(&inputs, false).unwrap();
        assert_eq!(p.layer_builds(), 3, "{}: new webcam frame", a.name);
        inputs.layout.width = 0.6;
        inputs.layout.style = OverlayStyle::Box;
        p.produce(&inputs, false).unwrap();
        assert_eq!(p.layer_builds(), 4, "{}: Box", a.name);
        inputs.layout.width = 0.3;
        p.produce(&inputs, false).unwrap();
        assert_eq!(p.layer_builds(), 5, "{}: resize", a.name);
        // Unchanged inputs: nothing rebuilt (and the drain/None semantics hold).
        for _ in 0..4 {
            p.produce(&inputs, false).unwrap();
        }
        assert_eq!(p.layer_builds(), 5, "{}: unchanged", a.name);
        // The moved, cached layer still matches the CPU compositor.
        let page_arc = Arc::new(page.clone());
        for (l, m) in [
            (
                layout(OverlayStyle::CutoutSmall, 0.3, 0.0, 0.4),
                Some(&mask),
            ),
            (
                layout(OverlayStyle::CutoutSmall, 0.9, 0.0, 0.4),
                Some(&mask),
            ),
        ] {
            let inputs = TickInputs {
                page: None,
                cam: Some(cam.clone()),
                mask: m.cloned(),
                layout: l,
                backdrop: p2c_compositor::Backdrop::default(),
            };
            let got = p.render_bgra_for_test(&inputs).unwrap();
            let mut cpu = Frame::new_black(640, 360);
            let pip = Pip {
                cam: &cam,
                mask: m,
                layout: &l,
            };
            Compositor::new().compose(
                Some(&page_arc),
                Some(pip),
                &p2c_compositor::Backdrop::default(),
                &mut cpu,
            );
            let (d, x, y) = bgra_diff(&cpu, &got);
            let mean = bgra_mean_diff(&cpu, &got);
            println!(
                "cached layer at x={} on {}: max {d}, mean {mean:.3?}",
                l.x, a.name
            );
            assert!(d <= SCALED_PIP.max, "{}: diff {d} at ({x},{y})", a.name);
            assert!(mean.iter().all(|m| *m <= SCALED_PIP.mean), "{}", a.name);
        }
    }
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
                backdrop: p2c_compositor::Backdrop::default(),
            })
            .unwrap();
        let mut cpu = Frame::new_black(640, 360);
        let pip = Pip {
            cam: &cam,
            mask: None,
            layout: &l,
        };
        Compositor::new().compose(
            None,
            Some(pip),
            &p2c_compositor::Backdrop::default(),
            &mut cpu,
        );
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
            backdrop: p2c_compositor::Backdrop::default(),
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
            backdrop: p2c_compositor::Backdrop::default(),
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

/// The CPU compositor's NV12 output for one PIP over `page`.
fn cpu_nv12(page: &Arc<Frame>, cam: &Arc<Frame>, mask: &Arc<Mask>, l: &PipLayout) -> Nv12Frame {
    let mut f = Frame::new_black(page.width, page.height);
    let pip = Pip {
        cam,
        mask: Some(mask),
        layout: l,
    };
    Compositor::new().compose(
        Some(page),
        Some(pip),
        &p2c_compositor::Backdrop::default(),
        &mut f,
    );
    bgra_to_nv12(&f, f.width, f.height)
}

#[test]
#[ignore]
fn gpu_hidden_pip_skips_new_inputs_then_shows_them() {
    let page = noise(640, 360, 61);
    let page_arc = Arc::new(page.clone());
    let (cam_a, cam_b) = (Arc::new(noise(320, 180, 62)), Arc::new(noise(320, 180, 63)));
    let mask_a = soft_mask(160, 90, EdgeRange { lo: 40, hi: 220 });
    let mut mask_b = (*soft_mask(160, 90, EdgeRange::FULL)).clone();
    mask_b.data.reverse();
    let mask_b = Arc::new(mask_b);
    let shown = layout(OverlayStyle::CutoutSmall, 0.3, 0.0, 0.4);
    let hidden = PipLayout {
        visible: false,
        ..shown
    };
    let want_new = cpu_nv12(&page_arc, &cam_b, &mask_b, &shown);
    let want_old = cpu_nv12(&page_arc, &cam_a, &mask_a, &shown);
    for a in hardware() {
        let gpu = Gpu::new(a.luid).unwrap();
        let mut p = producer_with_page(&gpu, &page, spec(640, 360));
        let inputs = |cam: &Arc<Frame>, mask: &Arc<Mask>, layout| TickInputs {
            page: None,
            cam: Some(cam.clone()),
            mask: Some(mask.clone()),
            layout,
            backdrop: p2c_compositor::Backdrop::default(),
        };
        // Hidden: one composite, drained out of the ring.
        let first = inputs(&cam_a, &mask_a, hidden);
        for _ in 0..3 {
            p.produce(&first, false).unwrap();
        }
        // A new webcam frame and mask behind the hidden PIP: no work.
        let behind = inputs(&cam_b, &mask_b, hidden);
        for _ in 0..3 {
            assert!(
                p.produce(&behind, false).unwrap().is_none(),
                "{}: hidden PIP inputs composited",
                a.name
            );
        }
        // Shown: the output carries the new frame and mask.
        let show = inputs(&cam_b, &mask_b, shown);
        let mut last = None;
        for _ in 0..3 {
            if let Some(o) = p.produce(&show, false).unwrap() {
                last = Some(o);
            }
        }
        let nv12 = last.and_then(|o| o.nv12).expect("an output once shown");
        let (ny, nuv) = max_diff(&want_new, &nv12);
        let (oy, ouv) = max_diff(&want_old, &nv12);
        println!(
            "hidden then shown on {}: vs new inputs Y {ny} UV {nuv}, vs old Y {oy} UV {ouv}",
            a.name
        );
        assert!(
            ny <= SCALED_PIP.max && nuv <= SCALED_PIP.max,
            "{}: shown output differs from the CPU with the new inputs: Y {ny} UV {nuv}",
            a.name
        );
        assert!(oy > 50, "{}: output still shows the old inputs", a.name);
        // And the BGRA composite matches the CPU path like the parity tests.
        let got = p.render_bgra_for_test(&show).unwrap();
        let mut cpu = Frame::new_black(640, 360);
        let pip = Pip {
            cam: &cam_b,
            mask: Some(&mask_b),
            layout: &shown,
        };
        Compositor::new().compose(
            Some(&page_arc),
            Some(pip),
            &p2c_compositor::Backdrop::default(),
            &mut cpu,
        );
        let (d, x, y) = bgra_diff(&cpu, &got);
        let mean = bgra_mean_diff(&cpu, &got);
        assert!(d <= SCALED_PIP.max, "{}: diff {d} at ({x},{y})", a.name);
        assert!(mean.iter().all(|m| *m <= SCALED_PIP.mean), "{}", a.name);
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
                backdrop: p2c_compositor::Backdrop::default(),
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

/// GPU time per pass (D3D11 timestamps) of a live-like 1080p cut-out-full run:
/// the page changes every tick (animated page), webcam and mask on 2 ticks of
/// 3 (~20/s at 30 fps), a preview is due every other tick.
#[test]
#[ignore]
fn bench_compose_gpu_passes_1080p() {
    let page = noise(1920, 1080, 31);
    let cams: Vec<_> = (0..4).map(|i| Arc::new(noise(1280, 720, 32 + i))).collect();
    let masks: Vec<_> = (0..4)
        .map(|i| {
            soft_mask(
                640,
                360,
                EdgeRange {
                    lo: 40,
                    hi: 220 + i,
                },
            )
        })
        .collect();
    for (style, width) in [
        (OverlayStyle::CutoutFull, 0.25),
        (OverlayStyle::CutoutSmall, 0.3),
        (OverlayStyle::Box, 0.25),
    ] {
        for a in hardware() {
            let gpu = Gpu::new(a.luid).unwrap();
            let input = PageInput::new(&gpu, 1920, 1080).unwrap();
            let shared = gpu.create_shared_bgra(&page).unwrap();
            let mut p = GpuProducer::new(gpu.clone(), input.clone(), spec(1920, 1080)).unwrap();
            p.set_profiling(true);
            let l = layout(style, 0.5, 1.0, width);
            let mut passes: Vec<(&'static str, Vec<f64>)> = Vec::new();
            let mut totals = Vec::new();
            let mut cam = cams[0].clone();
            let mut mask = masks[0].clone();
            for i in 0..150usize {
                input.on_shared_texture(shared.handle()).unwrap();
                if i % 3 != 2 {
                    cam = Arc::new((*cams[i % 4]).clone());
                    mask = masks[i % 4].clone();
                }
                let inputs = TickInputs {
                    page: None,
                    cam: Some(cam.clone()),
                    mask: Some(mask.clone()),
                    layout: l,
                    backdrop: p2c_compositor::Backdrop::default(),
                };
                p.produce(&inputs, i % 2 == 0).unwrap();
                let Some(prof) = p.take_profile() else {
                    continue;
                };
                if i < 20 {
                    continue;
                }
                totals.push(prof.iter().map(|(_, ms)| ms).sum::<f64>());
                for (name, ms) in prof {
                    match passes.iter_mut().find(|(n, _)| *n == name) {
                        Some((_, v)) => v.push(ms),
                        None => passes.push((name, vec![ms])),
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let med = |v: &mut Vec<f64>| {
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                v[v.len() / 2]
            };
            let p95 = |v: &Vec<f64>| v[((v.len() - 1) as f64 * 0.95) as usize];
            let mut line = String::new();
            for (name, v) in &mut passes {
                let m = med(v);
                line += &format!(" {name} {m:.3}");
            }
            let total = med(&mut totals);
            println!(
                "gpu passes {style:?} on {}: total median {total:.3} ms p95 {:.3} ms (~{:.1}% at 30 fps) |{line} (ms, median per tick)",
                a.name,
                p95(&totals),
                total * 30.0 / 10.0,
            );
        }
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
        backdrop: p2c_compositor::Backdrop::default(),
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
        Arc::default(),
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
        Arc::default(),
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

// ---- Backdrops and refined edges (quality-backgrounds Task 4) ----
//
// Not ignored: they run on every hardware adapter of this machine and skip
// with a log line where there is none.

use p2c_compositor::{Backdrop, BoxBackdrop, ComposeNote, FullBackdrop};

/// Hardware adapters, or none (with a log line) so the caller skips.
fn adapters_or_skip(test: &str) -> Vec<AdapterInfo> {
    let l: Vec<_> = enumerate().into_iter().filter(|a| !a.software).collect();
    if l.is_empty() {
        println!("{test}: no hardware D3D11 adapter; skipped");
    }
    l
}

/// Panics, naming where and how many, if any output pixel isn't opaque.
fn assert_opaque(f: &Frame, what: &str) {
    let px = f.data.as_chunks::<4>().0;
    let bad: Vec<usize> = (0..px.len()).filter(|&i| px[i][3] != 255).collect();
    if let Some(&i) = bad.first() {
        let (x, y) = (i as u32 % f.width, i as u32 / f.width);
        let last = *bad.last().unwrap() as u32;
        panic!(
            "{what}: {} of {} pixels not opaque; first ({x},{y}) {:?}, last ({},{})",
            bad.len(),
            px.len(),
            px[i],
            last % f.width,
            last / f.width
        );
    }
}

fn fnv(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// (label, page, output size, inputs) of the GPU compositor's existing cases
/// with `Backdrop::default()` (Page, Room, LUT edges).
/// (label, page, output size, inputs).
type TodayCase = (&'static str, Option<Frame>, (u32, u32), TickInputs);

fn today_cases() -> Vec<TodayCase> {
    let ti = |cam: Option<Frame>, mask: Option<Arc<Mask>>, l: PipLayout| TickInputs {
        page: None,
        cam: cam.map(Arc::new),
        mask,
        layout: l,
        backdrop: Backdrop::default(),
    };
    let mut edge100 = Mask::new(40, 30, vec![100; 40 * 30]).unwrap();
    edge100.edge = EdgeRange { lo: 64, hi: 192 };
    vec![
        (
            "box",
            Some(noise(640, 360, 1)),
            (640, 360),
            ti(
                Some(noise(320, 240, 2)),
                None,
                layout(OverlayStyle::Box, 0.3, 0.6, 0.3),
            ),
        ),
        (
            "box with a mask",
            Some(noise(640, 360, 1)),
            (640, 360),
            ti(
                Some(noise(320, 240, 2)),
                Some(soft_mask(80, 60, EdgeRange::FULL)),
                layout(OverlayStyle::Box, 0.3, 0.6, 0.3),
            ),
        ),
        (
            "cutout small",
            Some(noise(640, 360, 3)),
            (640, 360),
            ti(
                Some(noise(320, 180, 4)),
                Some(soft_mask(80, 45, EdgeRange::FULL)),
                layout(OverlayStyle::CutoutSmall, 0.7, 0.0, 0.4),
            ),
        ),
        (
            "cutout full x=0",
            Some(noise(640, 360, 5)),
            (640, 360),
            ti(
                Some(noise(320, 180, 6)),
                Some(soft_mask(160, 90, EdgeRange::FULL)),
                layout(OverlayStyle::CutoutFull, 0.0, 0.0, 0.25),
            ),
        ),
        (
            "edge range, mask 100",
            Some(noise(640, 360, 7)),
            (640, 360),
            ti(
                Some(noise(320, 240, 8)),
                Some(Arc::new(edge100)),
                layout(OverlayStyle::CutoutFull, 0.5, 0.0, 0.25),
            ),
        ),
        (
            "edge range, soft mask",
            Some(noise(640, 360, 9)),
            (640, 360),
            ti(
                Some(noise(320, 240, 10)),
                Some(soft_mask(64, 48, EdgeRange { lo: 64, hi: 192 })),
                layout(OverlayStyle::CutoutSmall, 0.2, 0.0, 0.5),
            ),
        ),
        (
            "cutout full 1080p",
            Some(noise(1920, 1080, 41)),
            (1920, 1080),
            ti(
                Some(noise(1280, 720, 42)),
                Some(soft_mask(640, 360, EdgeRange { lo: 40, hi: 220 })),
                layout(OverlayStyle::CutoutFull, 0.5, 0.0, 0.25),
            ),
        ),
        (
            "cut-out without mask",
            Some(noise(640, 360, 11)),
            (640, 360),
            ti(
                Some(noise(320, 180, 12)),
                None,
                layout(OverlayStyle::CutoutFull, 0.2, 0.5, 0.3),
            ),
        ),
        (
            "4:3 page",
            Some(noise(400, 300, 13)),
            (640, 360),
            ti(
                Some(noise(160, 120, 14)),
                None,
                layout(OverlayStyle::Box, 1.0, 1.0, 0.25),
            ),
        ),
        (
            "4:3 page, downscaled, no webcam",
            Some(noise(1024, 768, 15)),
            (640, 360),
            ti(None, None, PipLayout::default()),
        ),
        (
            "hidden pip",
            Some(noise(640, 360, 17)),
            (640, 360),
            ti(
                Some(noise(320, 180, 18)),
                Some(soft_mask(80, 45, EdgeRange::FULL)),
                PipLayout {
                    visible: false,
                    ..layout(OverlayStyle::CutoutSmall, 0.5, 0.0, 0.4)
                },
            ),
        ),
        (
            "no page yet",
            None,
            (640, 360),
            ti(
                Some(noise(320, 180, 16)),
                None,
                layout(OverlayStyle::Box, 0.0, 0.0, 0.25),
            ),
        ),
    ]
}

/// GPU output hashes of [`today_cases`] from the GPU compositor before
/// backdrops (commit 5296130), per (adapter name, user-mode driver version).
/// Another driver may round differently inside the same shaders: an unknown
/// adapter or driver prints its hashes and skips (never fails).
const TODAY_GPU: &[(&str, &str, &[u64])] = &[
    (
        "Intel(R) UHD Graphics",
        "32.0.101.7088",
        &[
            4025485033438984934,
            4025485033438984934,
            192081244833896544,
            8344175307028417851,
            3667518654555366198,
            8357457479091645205,
            5066645306656391881,
            12231197240143780934,
            6736479082463568632,
            2842473286698388443,
            294307805838554806,
            3858574375304064849,
        ],
    ),
    (
        "NVIDIA RTX 2000 Ada Generation Laptop GPU",
        "32.0.15.9658",
        &[
            16729803995776210676,
            16729803995776210676,
            729809452394762057,
            8344175307028417851,
            7087019108189793500,
            8357457479091645205,
            10751377556055003541,
            3808767700574327800,
            14294963803269799104,
            209133133403119282,
            294307805838554806,
            3858574375304064849,
        ],
    ),
];

/// Refined edges off with Page/Room: exactly what the GPU drew before backdrops.
#[test]
fn gpu_refined_off_matches_previous_gpu_output() {
    for a in adapters_or_skip("gpu_refined_off_matches_previous_gpu_output") {
        let gpu = Gpu::new(a.luid).unwrap();
        let mut got = Vec::new();
        for (label, page, (w, h), inputs) in today_cases() {
            let mut p = match &page {
                Some(pg) => producer_with_page(&gpu, pg, spec(w, h)),
                None => {
                    let input = PageInput::new(&gpu, w, h).unwrap();
                    GpuProducer::new(gpu.clone(), input, spec(w, h)).unwrap()
                }
            };
            let out = p.render_bgra_for_test(&inputs).unwrap();
            assert_opaque(&out, &format!("{label} on {}", a.name));
            got.push((label, fnv(&out.data)));
        }
        let hashes: Vec<u64> = got.iter().map(|(_, h)| *h).collect();
        let driver = p2c_d3d::driver_version(a.luid).unwrap_or_else(|| "unknown".into());
        let known = TODAY_GPU
            .iter()
            .find(|(n, d, _)| *n == a.name && *d == driver);
        if known.is_none() || std::env::var_os("P2C_PRINT_GPU_HASHES").is_some() {
            println!("(\"{}\", \"{driver}\", &{hashes:?}),", a.name);
        }
        match known {
            Some((_, _, want)) => {
                for ((label, h), w) in got.iter().zip(want.iter()) {
                    assert_eq!(
                        h, w,
                        "{}: {label} differs from the previous GPU output",
                        a.name
                    );
                }
                assert_eq!(hashes.len(), want.len());
                println!(
                    "refined off = previous GPU output on {} {driver} (exact)",
                    a.name
                );
            }
            None => println!(
                "{} driver {driver}: no previous GPU hashes for it; skipped \
                 (CPU parity is checked by the other tests)",
                a.name
            ),
        }
    }
}

const T_PERSON: [u8; 4] = [180, 255, 255, 255];
const T_GREEN: [u8; 4] = [0, 255, 0, 255];
const T_BLUE: [u8; 4] = [255, 0, 0, 255];
const T_MAGENTA: [u8; 4] = [255, 0, 255, 255];
const T_WHITE: [u8; 4] = [255, 255, 255, 255];

/// The compositor tests' webcam: left half person (solid), right half room
/// (a 2 px green/blue checker, so sharp and blurred rooms differ a lot).
fn person_cam(w: u32, h: u32) -> Arc<Frame> {
    let mut d = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let px = if x < w / 2 {
                T_PERSON
            } else if (x / 2 + y / 2) % 2 == 0 {
                T_GREEN
            } else {
                T_BLUE
            };
            d.extend_from_slice(&px);
        }
    }
    Arc::new(Frame::from_bgra(w, h, d).unwrap())
}

/// Person mask matching `person_cam` (left half 255) at `w` x `h`.
fn person_mask_sized(w: u32, h: u32) -> Arc<Mask> {
    let data = (0..h)
        .flat_map(|_| (0..w).map(move |x| if x < w / 2 { 255 } else { 0 }))
        .collect();
    Arc::new(Mask::new(w, h, data).unwrap())
}

fn person_mask() -> Arc<Mask> {
    person_mask_sized(64, 36)
}

fn solid_arc(w: u32, h: u32, px: [u8; 4]) -> Arc<Frame> {
    Arc::new(Frame::from_bgra(w, h, px.repeat((w * h) as usize)).unwrap())
}

fn backdrop(full: FullBackdrop, in_box: BoxBackdrop, refined_edges: bool) -> Backdrop {
    Backdrop {
        full,
        in_box,
        refined_edges,
    }
}

/// The CPU compositor's output for `inputs` (page `page` or none) and its note.
fn cpu_compose(
    page: Option<&Frame>,
    (w, h): (u32, u32),
    inputs: &TickInputs,
) -> (Frame, ComposeNote) {
    let page = page.map(|p| Arc::new(p.clone()));
    let mut out = Frame::new_black(w, h);
    let mut c = Compositor::new();
    let pip = inputs.cam.as_ref().map(|cam| Pip {
        cam,
        mask: inputs.mask.as_ref(),
        layout: &inputs.layout,
    });
    c.compose(page.as_ref(), pip, &inputs.backdrop, &mut out);
    (out, c.note())
}

/// A producer showing `page` (or no page yet).
fn producer(gpu: &Gpu, page: Option<&Frame>, (w, h): (u32, u32)) -> GpuProducer {
    match page {
        Some(pg) => producer_with_page(gpu, pg, spec(w, h)),
        None => {
            let input = PageInput::new(gpu, w, h).unwrap();
            GpuProducer::new(gpu.clone(), input, spec(w, h)).unwrap()
        }
    }
}

/// Mean absolute difference of the worst colour channel, and the largest
/// single difference with its position.
fn mean_and_max(a: &Frame, b: &Frame) -> (f64, u8, u32, u32) {
    let mean = bgra_mean_diff(a, b);
    let (d, x, y) = bgra_diff(a, b);
    (mean[..3].iter().cloned().fold(0.0, f64::max), d, x, y)
}

/// Tolerance of the backdrop/refined passes against the CPU compositor.
const BACKDROP_MEAN: f64 = 3.0;
const BACKDROP_MAX: u8 = 24;

type Case = (String, Option<Frame>, (u32, u32), TickInputs);

/// Compare the GPU with the CPU on every adapter.
fn backdrop_parity(test: &str, cases: &[Case]) {
    let adapters = adapters_or_skip(test);
    if adapters.is_empty() {
        return;
    }
    let cpu: Vec<_> = cases
        .iter()
        .map(|(label, page, size, inputs)| {
            let (out, note) = cpu_compose(page.as_ref(), *size, inputs);
            // The ticker computes the note for both paths from the inputs alone.
            let want = ComposeNote::of(
                &inputs.backdrop,
                &inputs.layout,
                inputs.cam.is_some(),
                inputs.mask.is_some(),
            );
            assert_eq!(note, want, "{label}: note");
            out
        })
        .collect();
    let mut failures = Vec::new();
    for a in adapters {
        let gpu = Gpu::new(a.luid).unwrap();
        for ((label, page, size, inputs), want) in cases.iter().zip(&cpu) {
            let mut p = producer(&gpu, page.as_ref(), *size);
            let got = p.render_bgra_for_test(inputs).unwrap();
            let (mean, d, x, y) = mean_and_max(want, &got);
            let line = format!(
                "{test} | {label} | {} | mean {mean:.3} | max {d} at ({x},{y})",
                a.name
            );
            println!("{line}");
            assert_opaque(&got, &format!("{label} on {}", a.name));
            if mean > BACKDROP_MEAN || d > BACKDROP_MAX {
                failures.push(format!(
                    "{line}: cpu {:?} gpu {:?}",
                    want.pixel(x, y),
                    got.pixel(x, y)
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn ti(cam: &Arc<Frame>, mask: Option<&Arc<Mask>>, l: PipLayout, b: Backdrop) -> TickInputs {
    TickInputs {
        page: None,
        cam: Some(cam.clone()),
        mask: mask.cloned(),
        layout: l,
        backdrop: b,
    }
}

/// Task 3's `backdrop_table` cases (and the fallbacks of `plan()`), GPU vs CPU.
#[test]
fn gpu_backdrop_matches_cpu() {
    let white = Frame::from_bgra(320, 180, T_WHITE.repeat(320 * 180)).unwrap();
    let page = Some(white);
    let cam = person_cam(256, 144);
    let m = person_mask();
    let big_box = layout(OverlayStyle::Box, 0.0, 0.0, 0.8);
    let magenta = solid_arc(7, 5, T_MAGENTA);
    let picture = Arc::new(gradient(50, 20, 3));
    let mut cases: Vec<Case> = Vec::new();
    for in_box in [
        BoxBackdrop::Room,
        BoxBackdrop::Blur,
        BoxBackdrop::Image(magenta.clone()),
        BoxBackdrop::Image(picture.clone()),
        BoxBackdrop::Colour([10, 20, 30]),
    ] {
        for fresh in [true, false] {
            let name = match &in_box {
                BoxBackdrop::Image(i) => format!("Image {}x{}", i.width, i.height),
                b => format!("{b:?}"),
            };
            let b = backdrop(FullBackdrop::Page, in_box.clone(), false);
            cases.push((
                format!("box {name} fresh={fresh}"),
                page.clone(),
                (320, 180),
                ti(&cam, fresh.then_some(&m), big_box, b),
            ));
        }
    }
    // Blurred room behind a small cut-out, fresh and stale mask.
    let cam320 = person_cam(320, 180);
    let small = layout(OverlayStyle::CutoutSmall, 1.0, 0.0, 0.5);
    for fresh in [true, false] {
        let b = backdrop(FullBackdrop::BlurredRoom, BoxBackdrop::Room, false);
        cases.push((
            format!("blurred room fresh={fresh}"),
            page.clone(),
            (320, 180),
            ti(&cam320, fresh.then_some(&m), small, b),
        ));
    }
    // A Box under a blurred room: in-box Room is treated as Blur.
    for fresh in [true, false] {
        let b = backdrop(FullBackdrop::BlurredRoom, BoxBackdrop::Room, false);
        cases.push((
            format!("blurred room + box room fresh={fresh}"),
            page.clone(),
            (320, 180),
            ti(&cam, fresh.then_some(&m), big_box, b),
        ));
    }
    // A 4:3 webcam under a blurred room: cover-cropped; hidden PIP.
    let cam43 = person_cam(160, 120);
    let hidden = PipLayout {
        visible: false,
        ..PipLayout::default()
    };
    cases.push((
        "blurred room 4:3 hidden pip".into(),
        page.clone(),
        (320, 180),
        ti(
            &cam43,
            None,
            hidden,
            backdrop(FullBackdrop::BlurredRoom, BoxBackdrop::Room, false),
        ),
    ));
    // Blurred room without a webcam: the page.
    cases.push((
        "blurred room no webcam".into(),
        page.clone(),
        (320, 180),
        TickInputs {
            page: None,
            cam: None,
            mask: None,
            layout: small,
            backdrop: backdrop(FullBackdrop::BlurredRoom, BoxBackdrop::Blur, false),
        },
    ));
    // Cut-out styles ignore the box backdrop.
    let full = layout(OverlayStyle::CutoutFull, 0.3, 0.0, 0.5);
    cases.push((
        "cutout full ignores in-box blur".into(),
        page.clone(),
        (320, 180),
        ti(
            &cam,
            Some(&m),
            full,
            backdrop(FullBackdrop::Page, BoxBackdrop::Blur, false),
        ),
    ));
    // Live sizes: 1080p output, 1280x720 webcam, 640x360 mask.
    let page1080 = noise(1920, 1080, 51);
    let cam720 = person_cam(1280, 720);
    let m360 = person_mask_sized(640, 360);
    for in_box in [BoxBackdrop::Blur, BoxBackdrop::Colour([200, 100, 50])] {
        cases.push((
            format!("1080p box {in_box:?}"),
            Some(page1080.clone()),
            (1920, 1080),
            ti(
                &cam720,
                Some(&m360),
                layout(OverlayStyle::Box, 1.0, 1.0, 0.3),
                backdrop(FullBackdrop::Page, in_box, false),
            ),
        ));
    }
    cases.push((
        "1080p blurred room cutout full".into(),
        Some(page1080.clone()),
        (1920, 1080),
        ti(
            &cam720,
            Some(&m360),
            layout(OverlayStyle::CutoutFull, 0.5, 0.0, 0.25),
            backdrop(FullBackdrop::BlurredRoom, BoxBackdrop::Room, false),
        ),
    ));
    backdrop_parity("gpu_backdrop_matches_cpu", &cases);
}

/// A webcam with texture in both halves, so the guide matters.
fn textured_person_cam(w: u32, h: u32, salt: u32) -> Arc<Frame> {
    let n = noise(w, h, 77 + salt);
    let mut d = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let t = &n.data[i..i + 3];
            let px = if x < w / 2 {
                [150 + t[0] / 8, 220 + t[1] / 8, 220 + t[2] / 8, 255]
            } else {
                [t[0] / 3, t[1] / 3, t[2] / 3, 255]
            };
            d.extend_from_slice(&px);
        }
    }
    Arc::new(Frame::from_bgra(w, h, d).unwrap())
}

/// Refined edges on (guided upsampling replaces the edge LUT), GPU vs CPU.
#[test]
fn gpu_refined_matches_cpu() {
    let mut cases: Vec<Case> = Vec::new();
    let page = Some(noise(640, 360, 61));
    let cam = textured_person_cam(320, 180, 0);
    let soft = soft_mask(64, 36, EdgeRange { lo: 40, hi: 220 });
    let sharp = person_mask();
    for (style, x, w) in [
        (OverlayStyle::CutoutFull, 0.5, 0.25),
        (OverlayStyle::CutoutFull, 0.0, 0.25),
        (OverlayStyle::CutoutSmall, 0.3, 0.4),
        (OverlayStyle::Box, 0.6, 0.5),
    ] {
        for (mname, m) in [("soft", &soft), ("person", &sharp)] {
            let in_box = if style == OverlayStyle::Box {
                BoxBackdrop::Blur
            } else {
                BoxBackdrop::Room
            };
            cases.push((
                format!("{style:?} x={x} {mname} mask"),
                page.clone(),
                (640, 360),
                ti(
                    &cam,
                    Some(m),
                    layout(style, x, 1.0, w),
                    backdrop(FullBackdrop::Page, in_box, true),
                ),
            ));
        }
    }
    // A PIP downscaled from the webcam (the guide is scaled down).
    let cam_big = textured_person_cam(1280, 720, 1);
    cases.push((
        "small cut-out, 1280x720 webcam".into(),
        page.clone(),
        (640, 360),
        ti(
            &cam_big,
            Some(&soft),
            layout(OverlayStyle::CutoutSmall, 1.0, 1.0, 0.3),
            backdrop(FullBackdrop::Page, BoxBackdrop::Room, true),
        ),
    ));
    // Live sizes: the 1080p cut-out full is 1920 wide (refined at 960, then upscaled).
    let m360 = person_mask_sized(640, 360);
    for full in [FullBackdrop::Page, FullBackdrop::BlurredRoom] {
        cases.push((
            format!("1080p cutout full {full:?}"),
            Some(noise(1920, 1080, 62)),
            (1920, 1080),
            ti(
                &cam_big,
                Some(&m360),
                layout(OverlayStyle::CutoutFull, 0.5, 0.0, 0.25),
                backdrop(full, BoxBackdrop::Room, true),
            ),
        ));
    }
    // Refined without a mask: the same as off (a Box).
    cases.push((
        "refined, stale mask".into(),
        page.clone(),
        (640, 360),
        ti(
            &cam,
            None,
            layout(OverlayStyle::CutoutSmall, 0.3, 0.0, 0.4),
            backdrop(FullBackdrop::Page, BoxBackdrop::Room, true),
        ),
    ));
    backdrop_parity("gpu_refined_matches_cpu", &cases);
    // The comparison can tell: the GPU's refined matte is far from the LUT one.
    let (_, page0, size0, refined0) = &cases[0];
    let mut lut = refined0.clone();
    lut.backdrop.refined_edges = false;
    let (cpu_lut, _) = cpu_compose(page0.as_ref(), *size0, &lut);
    for a in adapters_or_skip("gpu_refined_matches_cpu") {
        let gpu = Gpu::new(a.luid).unwrap();
        let got = producer(&gpu, page0.as_ref(), *size0)
            .render_bgra_for_test(refined0)
            .unwrap();
        let (_, d, _, _) = mean_and_max(&cpu_lut, &got);
        assert!(
            d > BACKDROP_MAX,
            "{}: refined looks like the LUT matte ({d})",
            a.name
        );
    }
}

/// The blurs are rebuilt for a new webcam frame or a new size (webcam
/// reconnect, PIP resize), never shown stale at the wrong size, and reused
/// otherwise.
#[test]
fn gpu_blur_cache_rebuilds_on_size_change() {
    let page = noise(640, 360, 71);
    for a in adapters_or_skip("gpu_blur_cache_rebuilds_on_size_change") {
        let gpu = Gpu::new(a.luid).unwrap();
        let input = PageInput::new(&gpu, 640, 360).unwrap();
        let shared = gpu.create_shared_bgra(&page).unwrap();
        input.on_shared_texture(shared.handle()).unwrap();
        let mut p = GpuProducer::new(gpu.clone(), input.clone(), spec(640, 360)).unwrap();
        let cam = person_cam(320, 180);
        let m = person_mask();
        let mut inputs = ti(
            &cam,
            Some(&m),
            layout(OverlayStyle::Box, 0.2, 0.3, 0.4),
            backdrop(FullBackdrop::Page, BoxBackdrop::Blur, false),
        );
        let check = |p: &mut GpuProducer, inputs: &TickInputs, what: &str| {
            let got = p.render_bgra_for_test(inputs).unwrap();
            let (want, _) = cpu_compose(Some(&page), (640, 360), inputs);
            let (mean, d, x, y) = mean_and_max(&want, &got);
            println!("{what} on {}: mean {mean:.3}, max {d}", a.name);
            assert!(
                mean <= BACKDROP_MEAN && d <= BACKDROP_MAX,
                "{what} on {}: mean {mean:.3}, max {d} at ({x},{y})",
                a.name
            );
        };
        // Page-only changes and moves reuse the blur.
        for _ in 0..3 {
            input.on_shared_texture(shared.handle()).unwrap();
            p.produce(&inputs, false).unwrap();
        }
        assert_eq!(p.blur_builds(), 1, "{}: page-only changes", a.name);
        inputs.layout.x = 0.9;
        p.produce(&inputs, false).unwrap();
        assert_eq!(p.blur_builds(), 1, "{}: move", a.name);
        // Resize: rebuilt at the new size.
        inputs.layout.width = 0.25;
        p.produce(&inputs, false).unwrap();
        assert_eq!(p.blur_builds(), 2, "{}: resize", a.name);
        check(&mut p, &inputs, "after resize");
        // A new webcam frame at the same size.
        inputs.cam = Some(person_cam(320, 180));
        p.produce(&inputs, false).unwrap();
        assert_eq!(p.blur_builds(), 3, "{}: new frame", a.name);
        // The webcam reconnects at another size (4:3): new PIP size, new blur.
        inputs.cam = Some(person_cam(200, 150));
        p.produce(&inputs, false).unwrap();
        assert_eq!(p.blur_builds(), 4, "{}: reconnect", a.name);
        check(&mut p, &inputs, "after reconnect");
        // Back to Room: no blur needed; then a blurred room (its own cache).
        inputs.backdrop.in_box = BoxBackdrop::Room;
        p.produce(&inputs, false).unwrap();
        assert_eq!(p.blur_builds(), 4, "{}: room", a.name);
        inputs.backdrop.full = FullBackdrop::BlurredRoom;
        inputs.layout.style = OverlayStyle::CutoutSmall;
        p.produce(&inputs, false).unwrap();
        assert_eq!(p.blur_builds(), 5, "{}: blurred room", a.name);
        for _ in 0..3 {
            input.on_shared_texture(shared.handle()).unwrap();
            p.produce(&inputs, false).unwrap();
        }
        assert_eq!(p.blur_builds(), 5, "{}: page ignored", a.name);
        inputs.cam = Some(person_cam(320, 180));
        p.produce(&inputs, false).unwrap();
        assert_eq!(p.blur_builds(), 6, "{}: blurred room, new frame", a.name);
        check(&mut p, &inputs, "blurred room after a size change");
    }
}

/// GPU time per frame (D3D11 timestamps) at 1080p with every input new on
/// every tick (worst case): Quality-size mask, refined edges, blur.
#[test]
#[ignore = "GPU timing; don't run while the user's Page2Cam runs"]
fn gpu_refined_and_blur_within_budget() {
    let page = noise(1920, 1080, 81);
    let cams: Vec<_> = (0..4).map(|i| textured_person_cam(1280, 720, i)).collect();
    let masks: Vec<_> = (0..4)
        .map(|i| {
            soft_mask(
                640,
                360,
                EdgeRange {
                    lo: 40,
                    hi: 220 + i,
                },
            )
        })
        .collect();
    let configs = [
        (
            "blurred room + cut-out full",
            layout(OverlayStyle::CutoutFull, 0.5, 0.0, 0.25),
            FullBackdrop::BlurredRoom,
            BoxBackdrop::Room,
        ),
        (
            "box 0.3 + in-box blur",
            layout(OverlayStyle::Box, 1.0, 1.0, 0.3),
            FullBackdrop::Page,
            BoxBackdrop::Blur,
        ),
        (
            "cut-out small on the page",
            layout(OverlayStyle::CutoutSmall, 0.5, 1.0, 0.3),
            FullBackdrop::Page,
            BoxBackdrop::Room,
        ),
    ];
    let mut over = Vec::new();
    for a in adapters_or_skip("gpu_refined_and_blur_within_budget") {
        for (name, l, full, in_box) in &configs {
            let gpu = Gpu::new(a.luid).unwrap();
            let input = PageInput::new(&gpu, 1920, 1080).unwrap();
            let shared = gpu.create_shared_bgra(&page).unwrap();
            let mut p = GpuProducer::new(gpu.clone(), input.clone(), spec(1920, 1080)).unwrap();
            p.set_profiling(true);
            let mut totals = Vec::new();
            let mut passes: Vec<(&'static str, Vec<f64>)> = Vec::new();
            for i in 0..120usize {
                input.on_shared_texture(shared.handle()).unwrap();
                let inputs = TickInputs {
                    page: None,
                    cam: Some(Arc::new((*cams[i % 4]).clone())),
                    mask: Some(masks[i % 4].clone()),
                    layout: *l,
                    backdrop: backdrop(*full, in_box.clone(), true),
                };
                p.produce(&inputs, false).unwrap();
                let Some(prof) = p.take_profile() else {
                    continue;
                };
                if i < 20 {
                    continue;
                }
                totals.push(prof.iter().map(|(_, ms)| ms).sum::<f64>());
                for (n, ms) in prof {
                    match passes.iter_mut().find(|(k, _)| *k == n) {
                        Some((_, v)) => v.push(ms),
                        None => passes.push((n, vec![ms])),
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let med = |v: &mut Vec<f64>| {
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                v[v.len() / 2]
            };
            let mut line = String::new();
            for (n, v) in &mut passes {
                line += &format!(" {n} {:.3}", med(v));
            }
            let total = med(&mut totals);
            let p95 = totals[((totals.len() - 1) as f64 * 0.95) as usize];
            println!(
                "budget | {name} | {} | GPU median {total:.3} ms, p95 {p95:.3} ms per frame |{line}",
                a.name
            );
            if total > 2.0 {
                over.push(format!("{name} on {}: {total:.3} ms", a.name));
            }
        }
    }
    assert!(over.is_empty(), "over the 2 ms budget: {over:?}");
}

/// If the backdrop shaders can't be compiled, Page/Room keeps working on
/// the GPU, but a backdrop that needs them (blur, image, colour, refined
/// edges) is handed to the CPU compositor at once: never the sharp room
/// under a blur setting.
#[test]
fn gpu_backdrop_shader_failure_hands_backdrops_to_cpu() {
    use p2c_compositor::{FrameSink, Output, ProducerEvent, Ticker};
    use p2c_core::{FrameSlot, MaskSlot};
    let page = noise(640, 360, 91);
    let cam = person_cam(320, 180);
    let m = person_mask();
    let box_l = layout(OverlayStyle::Box, 0.3, 0.5, 0.4);
    let small = layout(OverlayStyle::CutoutSmall, 1.0, 0.0, 0.5);
    let room = ti(&cam, Some(&m), box_l, Backdrop::default());
    let needs: [(&str, TickInputs); 2] = [
        (
            "box + in-box blur",
            ti(
                &cam,
                Some(&m),
                box_l,
                backdrop(FullBackdrop::Page, BoxBackdrop::Blur, false),
            ),
        ),
        (
            "blurred room",
            ti(
                &cam,
                Some(&m),
                small,
                backdrop(FullBackdrop::BlurredRoom, BoxBackdrop::Room, false),
            ),
        ),
    ];
    for a in adapters_or_skip("gpu_backdrop_shader_failure_hands_backdrops_to_cpu") {
        let gpu = Gpu::new(a.luid).unwrap();
        // The producer alone: Page/Room drawn here; backdrops held + handed over.
        let mut p = producer(&gpu, Some(&page), (640, 360));
        p.fail_backdrop_shaders_for_test();
        p.produce(&room, false).unwrap();
        assert_eq!(p.producer_note(), None, "{}", a.name);
        assert!(p.fallback_request().is_none(), "{}", a.name);
        let got = p.render_bgra_for_test(&room).unwrap();
        let (want, _) = cpu_compose(Some(&page), (640, 360), &room);
        let (_, d, x, y) = mean_and_max(&want, &got);
        assert!(d <= 2, "{} page/room: {d} at ({x},{y})", a.name);
        for (what, inputs) in &needs {
            let out = p.produce(inputs, false).unwrap();
            assert!(out.is_none(), "{} {what}: drew without the shaders", a.name);
            assert_eq!(
                p.producer_note(),
                Some(ComposeNote::BackdropUnavailable),
                "{} {what}",
                a.name
            );
            let why = p.fallback_request().expect("hand-over requested");
            assert!(why.contains("shaders"), "{why}");
        }

        // In the ticker: the CPU compositor takes over and draws the backdrop.
        struct Rec(Arc<std::sync::Mutex<Vec<Output>>>);
        impl FrameSink for Rec {
            fn push(&mut self, out: &Output) -> anyhow::Result<()> {
                self.0.lock().unwrap().push(out.clone());
                Ok(())
            }
        }
        for (what, inputs) in &needs {
            let mut p = producer(&gpu, Some(&page), (640, 360));
            p.fail_backdrop_shaders_for_test();
            let page_slot = FrameSlot::new();
            page_slot.put(page.clone());
            let cam_slot = FrameSlot::new();
            cam_slot.put_arc(cam.clone());
            let mask_slot = MaskSlot::new();
            mask_slot.put_arc(m.clone());
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let (tx, rx) = std::sync::mpsc::channel();
            let t = Ticker::spawn(
                spec(640, 360),
                Some(page_slot),
                Some(cam_slot),
                Some(mask_slot),
                Arc::new(parking_lot::RwLock::new(inputs.layout)),
                Arc::new(parking_lot::RwLock::new(inputs.backdrop.clone())),
                Box::new(p),
                vec![Box::new(Rec(seen.clone()))],
                tx,
            );
            let ev = rx.recv_timeout(Duration::from_secs(3));
            std::thread::sleep(Duration::from_millis(200));
            t.stop();
            let ProducerEvent::Failed(reason) = ev.expect("no hand-over to the CPU");
            assert!(reason.contains("shaders"), "{reason}");
            let seen = seen.lock().unwrap();
            assert!(!seen.is_empty(), "{} {what}: no frames", a.name);
            // Every frame pushed is the CPU's backdrop frame (none from the GPU).
            let (cpu, _) = cpu_compose(Some(&page), (640, 360), inputs);
            let want = bgra_to_nv12(&cpu, 640, 360);
            for o in seen.iter() {
                let got = o.nv12.as_ref().expect("nv12");
                assert_eq!(got.data, want.data, "{} {what}: not the CPU frame", a.name);
            }
            println!("{what} on {}: handed to the CPU ({reason})", a.name);
        }
    }
}

/// Dragging the PIP's size (smaller, then larger again) reuses grown
/// textures and still matches the CPU at every size.
#[test]
fn gpu_resize_drag_matches_cpu() {
    let page = noise(640, 360, 92);
    let cam = textured_person_cam(320, 180, 5);
    let m = person_mask();
    let img = Arc::new(gradient(90, 40, 7));
    for a in adapters_or_skip("gpu_resize_drag_matches_cpu") {
        let gpu = Gpu::new(a.luid).unwrap();
        for in_box in [BoxBackdrop::Image(img.clone()), BoxBackdrop::Blur] {
            let mut p = producer(&gpu, Some(&page), (640, 360));
            for width in [0.6, 0.35, 0.45, 0.3, 0.62] {
                let inputs = ti(
                    &cam,
                    Some(&m),
                    layout(OverlayStyle::Box, 0.5, 0.5, width),
                    backdrop(FullBackdrop::Page, in_box.clone(), true),
                );
                p.produce(&inputs, false).unwrap();
                let got = p.render_bgra_for_test(&inputs).unwrap();
                let (want, _) = cpu_compose(Some(&page), (640, 360), &inputs);
                let (mean, d, x, y) = mean_and_max(&want, &got);
                assert!(
                    mean <= BACKDROP_MEAN && d <= BACKDROP_MAX,
                    "{} width {width}: mean {mean:.3}, max {d} at ({x},{y})",
                    a.name
                );
            }
        }
        println!("resize drag ok on {}", a.name);
    }
}

/// The test pages reach `PageInput` intact: a shared texture made by
/// `create_shared_bgra`, opened from its handle and copied (as CEF pages
/// are), equals the frame, also with other textures created right after.
/// With the frame given as the shared texture's initial data, NVIDIA (RTX
/// 2000 Ada, 32.0.15.9658) intermittently showed zeros or stale memory
/// (alpha 0) through the opened handle; this loop failed on every run.
#[test]
fn shared_test_pages_reach_the_page_input_intact() {
    let src = noise(640, 360, 101);
    let other = Frame::from_bgra(640, 360, [7u8, 7, 7, 0].repeat(640 * 360)).unwrap();
    for a in adapters_or_skip("shared_test_pages_reach_the_page_input_intact") {
        let gpu = Gpu::new(a.luid).unwrap();
        for i in 0..40 {
            let page = PageInput::new(&gpu, 640, 360).unwrap();
            let shared = gpu.create_shared_bgra(&src).unwrap();
            page.on_shared_texture(shared.handle()).unwrap();
            drop(shared);
            let more: Vec<_> = (0..4).map(|_| gpu.upload_bgra(&other).unwrap()).collect();
            let back = gpu.read_bgra(&page.texture()).unwrap();
            drop(more);
            let bad = back
                .data
                .iter()
                .zip(&src.data)
                .filter(|(a, b)| a != b)
                .count();
            assert_eq!(
                bad, 0,
                "{} run {i}: {bad} bytes differ from the page",
                a.name
            );
        }
        println!("page copy kept on {}", a.name);
    }
}
