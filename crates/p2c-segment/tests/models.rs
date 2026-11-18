//! Real models + real ONNX Runtime. Needs `scripts/fetch-models.sh` first.
use p2c_segment::{assets_dir, runtime};

fn ort_dir() -> std::path::PathBuf {
    assets_dir()
        .join("onnxruntime")
        .join(if cfg!(windows) { "windows" } else { "linux" })
}

fn describe(s: &ort::session::Session) -> (Vec<String>, Vec<String>) {
    for i in s.inputs() {
        println!("  in  {} {:?}", i.name(), i.dtype());
    }
    for o in s.outputs() {
        println!("  out {} {:?}", o.name(), o.dtype());
    }
    (
        s.inputs().iter().map(|i| i.name().to_string()).collect(),
        s.outputs().iter().map(|o| o.name().to_string()).collect(),
    )
}

#[test]
#[ignore = "needs scripts/fetch-models.sh"]
fn both_models_load_and_report_shapes() {
    runtime::init(&ort_dir()).unwrap();
    let models = assets_dir().join("models");

    println!("RVM:");
    let (rvm, dev) = runtime::session(&models.join("rvm_mobilenetv3_fp32.onnx"), false).unwrap();
    assert_eq!(dev, runtime::Device::Cpu);
    let (ins, outs) = describe(&rvm);
    for n in ["src", "r1i", "r2i", "r3i", "r4i", "downsample_ratio"] {
        assert!(ins.iter().any(|i| i == n), "RVM input {n} missing: {ins:?}");
    }
    for n in ["fgr", "pha", "r1o", "r2o", "r3o", "r4o"] {
        assert!(
            outs.iter().any(|o| o == n),
            "RVM output {n} missing: {outs:?}"
        );
    }

    println!("MediaPipe selfie:");
    let (mp, _) = runtime::session(&models.join("selfie_landscape.onnx"), false).unwrap();
    let (ins, outs) = describe(&mp);
    assert_eq!(ins.len(), 1);
    assert_eq!(outs.len(), 1);
}

fn person_frame() -> p2c_core::Frame {
    let img = image::open(assets_dir().join("models/test-person.png"))
        .unwrap()
        .to_rgba8();
    let (w, h) = img.dimensions();
    let mut data = img.into_raw();
    for px in data.as_chunks_mut::<4>().0 {
        px.swap(0, 2); // RGBA -> BGRA
        px[3] = 0;
    }
    p2c_core::Frame::from_bgra(w, h, data).unwrap()
}

/// Mean alpha over the box (x0..x1, y0..y1) given as fractions of the frame.
fn mean_alpha(f: &p2c_core::Frame, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let (w, h) = (f.width as f32, f.height as f32);
    let (mut sum, mut n) = (0u64, 0u64);
    for y in (y0 * h) as u32..(y1 * h) as u32 {
        for x in (x0 * w) as u32..(x1 * w) as u32 {
            sum += f.data[((y * f.width + x) * 4 + 3) as usize] as u64;
            n += 1;
        }
    }
    sum as f32 / n as f32
}

#[test]
#[ignore = "needs scripts/fetch-models.sh"]
fn engines_cut_out_person() {
    runtime::init(&ort_dir()).unwrap();
    let models = assets_dir().join("models");
    for choice in [
        p2c_segment::EngineChoice::Fast,
        p2c_segment::EngineChoice::Quality,
    ] {
        let mut engine = p2c_segment::open_engine(&models, choice).unwrap();
        let mut frame = person_frame();
        let mut took = std::time::Duration::ZERO;
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let (mask, mw, mh) = engine.infer(&frame).unwrap();
            took = t.elapsed();
            p2c_segment::tensor::write_alpha(&mut frame, &mask, mw, mh);
        }
        let centre = mean_alpha(&frame, 0.4, 0.4, 0.6, 0.6);
        let corners = [
            mean_alpha(&frame, 0.0, 0.0, 0.1, 0.1),
            mean_alpha(&frame, 0.9, 0.0, 1.0, 0.1),
            mean_alpha(&frame, 0.0, 0.9, 0.1, 1.0),
            mean_alpha(&frame, 0.9, 0.9, 1.0, 1.0),
        ];
        println!(
            "{choice}: {} ms on {}, centre {centre:.0}, corners {corners:?}",
            took.as_millis(),
            engine.device()
        );
        assert!(centre > 200.0, "{choice}: centre {centre}");
        for c in corners {
            assert!(c < 60.0, "{choice}: corner {c}");
        }
    }
}
