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
