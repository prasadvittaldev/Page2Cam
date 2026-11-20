//! Real models + real ONNX Runtime. Needs `scripts/fetch-models.sh` first.
use p2c_segment::{assets_dir, runtime};

fn ort_dir() -> std::path::PathBuf {
    assets_dir()
        .join("onnxruntime")
        .join(if cfg!(windows) { "windows" } else { "linux" })
}

/// The adapter the 0.3.0 behaviour used: Windows' high-performance GPU.
fn auto_gpu() -> Option<u32> {
    match p2c_d3d::choose(&p2c_d3d::enumerate(), &p2c_d3d::GpuSetting::Auto).0 {
        p2c_d3d::GpuChoice::Gpu(a) => Some(a.index),
        p2c_d3d::GpuChoice::Cpu { .. } => None,
    }
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
    let (rvm, dev) = runtime::session(
        &models.join("rvm_mobilenetv3_fp32.onnx"),
        None,
        runtime::ModelSize::Large,
    )
    .unwrap();
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
    let (mp, _) = runtime::session(
        &models.join("selfie_landscape.onnx"),
        None,
        runtime::ModelSize::Small,
    )
    .unwrap();
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

/// Mean mask value over the box (x0..x1, y0..y1) given as fractions of the mask.
fn mean_alpha(m: &p2c_core::Mask, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let (w, h) = (m.width as f32, m.height as f32);
    let (mut sum, mut n) = (0u64, 0u64);
    for y in (y0 * h) as u32..(y1 * h) as u32 {
        for x in (x0 * w) as u32..(x1 * w) as u32 {
            sum += m.data[(y * m.width + x) as usize] as u64;
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
        let mut engine = p2c_segment::open_engine(&models, choice, auto_gpu()).unwrap();
        let frame = person_frame();
        let mut took = std::time::Duration::ZERO;
        let mut mask = None;
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let (values, mw, mh) = engine.infer(&frame).unwrap();
            took = t.elapsed();
            mask = Some(
                p2c_segment::post::to_mask(&values, mw, mh, p2c_core::EdgeRange::FULL).unwrap(),
            );
        }
        let m = mask.unwrap();
        let centre = mean_alpha(&m, 0.4, 0.4, 0.6, 0.6);
        let corners = [
            mean_alpha(&m, 0.0, 0.0, 0.1, 0.1),
            mean_alpha(&m, 0.9, 0.0, 1.0, 0.1),
            mean_alpha(&m, 0.0, 0.9, 0.1, 1.0),
            mean_alpha(&m, 0.9, 0.9, 1.0, 1.0),
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

/// Times `run` (after a warm-up) and prints p50/p95; returns the p50.
fn bench(label: &str, mut run: impl FnMut()) -> std::time::Duration {
    for _ in 0..10 {
        run();
    }
    let mut stats = p2c_core::DurationStats::default();
    let cpu0 = process_cpu();
    let t0 = std::time::Instant::now();
    for _ in 0..100 {
        let t = std::time::Instant::now();
        run();
        stats.record(t.elapsed());
    }
    let wall = t0.elapsed().as_secs_f64();
    let cores = (process_cpu() - cpu0) / wall;
    let s = stats.summary().unwrap();
    println!("{label:<44} {s}  (~{cores:.1} cores busy)");
    s.p50
}

/// CPU seconds used by this process so far (user + kernel).
#[cfg(windows)]
fn process_cpu() -> f64 {
    use std::mem::MaybeUninit;
    #[repr(C)]
    struct FileTime(u32, u32);
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn GetProcessTimes(
            h: isize,
            c: *mut FileTime,
            e: *mut FileTime,
            k: *mut FileTime,
            u: *mut FileTime,
        ) -> i32;
    }
    let mut t: [MaybeUninit<FileTime>; 4] = [const { MaybeUninit::uninit() }; 4];
    let [c, e, k, u] = &mut t;
    // SAFETY: four valid out-pointers for the current process.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            c.as_mut_ptr(),
            e.as_mut_ptr(),
            k.as_mut_ptr(),
            u.as_mut_ptr(),
        );
        let v = |f: &MaybeUninit<FileTime>| {
            let f = f.assume_init_ref();
            ((f.1 as u64) << 32 | f.0 as u64) as f64 / 1e7
        };
        v(k) + v(u)
    }
}

#[cfg(not(windows))]
fn process_cpu() -> f64 {
    0.0
}

/// Old vs new session settings on this machine's real CPU/GPU.
/// `cargo test -p p2c-segment --test models bench -- --ignored --nocapture`
#[test]
#[ignore = "needs scripts/fetch-models.sh; benchmark"]
fn bench_engines() {
    use ort::session::Session;
    use ort::value::Tensor;
    runtime::init(&ort_dir()).unwrap();
    let models = assets_dir().join("models");
    let frame = person_frame();

    let selfie = models.join("selfie_landscape.onnx");
    let input = p2c_segment::tensor::to_rgb_f32_nchw(&frame, 256, 144);
    let run_selfie = |s: &mut Session| {
        let t = Tensor::from_array(([1usize, 3, 144, 256], input.clone())).unwrap();
        s.run(ort::inputs!["pixel_values" => t]).unwrap();
    };
    // Before: ONNX Runtime defaults (one spinning thread per core).
    let env = runtime::env().unwrap();
    let mut old = Session::builder(env)
        .unwrap()
        .commit_from_file(&selfie)
        .unwrap();
    bench("Fast, ORT default threads (before)", || {
        run_selfie(&mut old)
    });
    drop(old);
    let (mut new, _) = runtime::session(&selfie, None, runtime::ModelSize::Small).unwrap();
    bench("Fast, 2 threads, no spinning (after)", || {
        run_selfie(&mut new)
    });

    let mut fast =
        p2c_segment::open_engine(&models, p2c_segment::EngineChoice::Fast, auto_gpu()).unwrap();
    bench("Fast engine incl. resize 1280x720 -> 256x144", || {
        fast.infer(&frame).unwrap();
    });

    let mut q =
        p2c_segment::open_engine(&models, p2c_segment::EngineChoice::Quality, auto_gpu()).unwrap();
    let label = format!("Quality engine on {}", q.device());
    bench(&label, || {
        q.infer(&frame).unwrap();
    });

    #[cfg(windows)]
    {
        // Before: DirectML's default adapter (adapter 0).
        let rvm = models.join("rvm_mobilenetv3_fp32.onnx");
        let mut s = Session::builder(env)
            .unwrap()
            .with_memory_pattern(false)
            .unwrap()
            .with_parallel_execution(false)
            .unwrap()
            .with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()])
            .unwrap()
            .commit_from_file(&rvm)
            .unwrap();
        let src = p2c_segment::tensor::to_rgb_f32_nchw(&frame, 640, 360);
        bench("Quality on DirectML default adapter (before)", || {
            let z = || Tensor::from_array((vec![1i64, 1, 1, 1], vec![0.0f32])).unwrap();
            s.run(ort::inputs![
                "src" => Tensor::from_array(([1usize, 3, 360, 640], src.clone())).unwrap(),
                "r1i" => z(), "r2i" => z(), "r3i" => z(), "r4i" => z(),
                "downsample_ratio" => Tensor::from_array(([1usize], vec![0.4f32])).unwrap(),
            ])
            .unwrap();
        });
    }
}

/// RVM on each DirectML adapter choice, same code path (recurrent state fed
/// back, 640x360 input), 300 runs each so a GPU monitor can see who works.
/// `cargo test -p p2c-segment --test models bench_dml -- --ignored --nocapture`
#[cfg(windows)]
#[test]
#[ignore = "needs scripts/fetch-models.sh; benchmark"]
fn bench_dml_adapters() {
    use ort::ep::directml::{DeviceFilter, PerformancePreference};
    use ort::ep::DirectML;
    use ort::session::Session;
    use ort::value::Tensor;
    runtime::init(&ort_dir()).unwrap();
    let rvm = assets_dir().join("models/rvm_mobilenetv3_fp32.onnx");
    let src = p2c_segment::tensor::to_rgb_f32_nchw(&person_frame(), 640, 360);
    let configs: Vec<(&str, DirectML)> = vec![
        ("device_id 0", DirectML::default().with_device_id(0)),
        ("device_id 1", DirectML::default().with_device_id(1)),
        ("default preference", DirectML::default()),
        (
            "high performance",
            DirectML::default()
                .with_performance_preference(PerformancePreference::HighPerformance)
                .with_device_filter(DeviceFilter::Gpu),
        ),
    ];
    for (label, ep) in configs {
        let mut s = Session::builder(runtime::env().unwrap())
            .unwrap()
            .with_memory_pattern(false)
            .unwrap()
            .with_parallel_execution(false)
            .unwrap()
            .with_execution_providers([ep.build().error_on_failure()])
            .unwrap()
            .commit_from_file(&rvm)
            .unwrap();
        let mut rec: Vec<(Vec<i64>, Vec<f32>)> =
            (0..4).map(|_| (vec![1, 1, 1, 1], vec![0.0])).collect();
        let mut step = || {
            let t = |i: usize| Tensor::from_array((rec[i].0.clone(), rec[i].1.clone())).unwrap();
            let out = s
                .run(ort::inputs![
                    "src" => Tensor::from_array(([1usize, 3, 360, 640], src.clone())).unwrap(),
                    "r1i" => t(0), "r2i" => t(1), "r3i" => t(2), "r4i" => t(3),
                    "downsample_ratio" => Tensor::from_array(([1usize], vec![0.4f32])).unwrap(),
                ])
                .unwrap();
            let next: Vec<_> = ["r1o", "r2o", "r3o", "r4o"]
                .iter()
                .map(|n| {
                    let (sh, d) = out[*n].try_extract_tensor::<f32>().unwrap();
                    (sh.to_vec(), d.to_vec())
                })
                .collect();
            drop(out);
            rec = next;
        };
        println!("START {label} at {:?}", std::time::SystemTime::now());
        for _ in 0..2 {
            bench(&format!("RVM, DirectML {label}"), &mut step);
        }
    }
}

/// BT.601 limited-range NV12 of a BGRA frame (test input only).
fn to_nv12(f: &p2c_core::Frame) -> p2c_core::Nv12Frame {
    let (w, h) = (f.width as usize & !1, f.height as usize & !1);
    let px = |x: usize, y: usize| {
        let i = (y * f.width as usize + x) * 4;
        let (b, g, r) = (f.data[i] as f32, f.data[i + 1] as f32, f.data[i + 2] as f32);
        (r, g, b)
    };
    let mut data = Vec::with_capacity(w * h * 3 / 2);
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = px(x, y);
            data.push((16.0 + 0.257 * r + 0.504 * g + 0.098 * b).round() as u8);
        }
    }
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let (r, g, b) = px(x, y);
            data.push((128.0 - 0.148 * r - 0.291 * g + 0.439 * b).round() as u8);
            data.push((128.0 + 0.439 * r - 0.368 * g - 0.071 * b).round() as u8);
        }
    }
    p2c_core::Nv12Frame::new(w as u32, h as u32, data).unwrap()
}

/// The live path: Quality on the auto GPU fed a 1280x720 NV12 frame, with the
/// model time and the input prep time reported apart (and the CPU it costs).
/// `cargo test -p p2c-segment --test models bench_quality_nv12 -- --ignored --nocapture`
#[test]
#[ignore = "needs scripts/fetch-models.sh; benchmark"]
fn bench_quality_nv12() {
    runtime::init(&ort_dir()).unwrap();
    let models = assets_dir().join("models");
    let mut person = person_frame();
    if (person.width, person.height) != (1280, 720) {
        person = p2c_compositor::scale_frame_straight(&person, 1280, 720);
    }
    let nv12 = to_nv12(&person);
    let mut q =
        p2c_segment::open_engine(&models, p2c_segment::EngineChoice::Quality, auto_gpu()).unwrap();
    let mut prep = p2c_core::DurationStats::default();
    let mut model = p2c_core::DurationStats::default();
    let total = bench(&format!("Quality NV12 1280x720 on {}", q.device()), || {
        let t = std::time::Instant::now();
        q.infer_nv12(&nv12).unwrap();
        let all = t.elapsed();
        let p = q.take_prep().unwrap();
        prep.record(p);
        model.record(all.saturating_sub(p));
    });
    println!("  total p50 {total:?}");
    println!("  model {}", model.summary().unwrap());
    println!("  prep  {}", prep.summary().unwrap());
}

/// DirectML session thread options, A/B interleaved run by run on the auto
/// GPU so machine-wide load hits every variant the same.
/// `cargo test -p p2c-segment --test models bench_dml_threads -- --ignored --nocapture`
#[cfg(windows)]
#[test]
#[ignore = "needs scripts/fetch-models.sh; benchmark"]
fn bench_dml_threads() {
    use ort::session::Session;
    use ort::value::Tensor;
    runtime::init(&ort_dir()).unwrap();
    let rvm = assets_dir().join("models/rvm_mobilenetv3_fp32.onnx");
    let src = p2c_segment::tensor::to_rgb_f32_nchw(&person_frame(), 640, 360);
    let index = auto_gpu().expect("needs a GPU") as i32;
    let open = |threads: Option<usize>| {
        let mut b = Session::builder(runtime::env().unwrap())
            .unwrap()
            .with_memory_pattern(false)
            .unwrap()
            .with_parallel_execution(false)
            .unwrap();
        if let Some(n) = threads {
            b = b
                .with_intra_threads(n)
                .unwrap()
                .with_inter_threads(1)
                .unwrap()
                .with_intra_op_spinning(false)
                .unwrap()
                .with_inter_op_spinning(false)
                .unwrap();
        }
        b.with_execution_providers([ort::ep::DirectML::default()
            .with_device_id(index)
            .build()
            .error_on_failure()])
            .unwrap()
            .commit_from_file(&rvm)
            .unwrap()
    };
    type Rec = Vec<(Vec<i64>, Vec<f32>)>;
    let mut variants: Vec<(&str, Session, Rec, p2c_core::DurationStats, f64)> = [
        ("ORT default threads (before)", None),
        ("1 thread, no spinning", Some(1)),
        ("2 threads, no spinning", Some(2)),
    ]
    .into_iter()
    .map(|(l, n)| {
        let rec = (0..4).map(|_| (vec![1, 1, 1, 1], vec![0.0])).collect();
        (l, open(n), rec, p2c_core::DurationStats::default(), 0.0)
    })
    .collect();
    let step = |s: &mut Session, rec: &mut Rec| {
        let t = |i: usize| Tensor::from_array((rec[i].0.clone(), rec[i].1.clone())).unwrap();
        let out = s
            .run(ort::inputs![
                "src" => Tensor::from_array(([1usize, 3, 360, 640], src.clone())).unwrap(),
                "r1i" => t(0), "r2i" => t(1), "r3i" => t(2), "r4i" => t(3),
                "downsample_ratio" => Tensor::from_array(([1usize], vec![0.4f32])).unwrap(),
            ])
            .unwrap();
        let next: Vec<_> = ["r1o", "r2o", "r3o", "r4o"]
            .iter()
            .map(|n| {
                let (sh, d) = out[*n].try_extract_tensor::<f32>().unwrap();
                (sh.to_vec(), d.to_vec())
            })
            .collect();
        drop(out);
        *rec = next;
    };
    for _ in 0..10 {
        for (_, s, rec, _, _) in variants.iter_mut() {
            step(s, rec);
        }
    }
    for _ in 0..100 {
        for (_, s, rec, stats, cpu) in variants.iter_mut() {
            let c = process_cpu();
            let t = std::time::Instant::now();
            step(s, rec);
            stats.record(t.elapsed());
            *cpu += process_cpu() - c;
        }
    }
    for (label, _, _, stats, cpu) in &variants {
        println!(
            "RVM DirectML {label:<30} {}  (CPU {:.1} ms/run)",
            stats.summary().unwrap(),
            cpu * 1000.0 / 100.0
        );
    }
}

/// The engine's recurrent-state feedback gives the same masks, frame after
/// frame, as a plain loop that copies every state out and back in (CPU, so
/// the runs are deterministic).
#[test]
#[ignore = "needs scripts/fetch-models.sh"]
fn rvm_state_feedback_matches_reference() {
    use ort::value::Tensor;
    runtime::init(&ort_dir()).unwrap();
    let models = assets_dir().join("models");
    let frame = person_frame();
    let mut engine =
        p2c_segment::open_engine(&models, p2c_segment::EngineChoice::Quality, None).unwrap();
    let (mut s, _) = runtime::session(
        &models.join("rvm_mobilenetv3_fp32.onnx"),
        None,
        runtime::ModelSize::Large,
    )
    .unwrap();
    let src = p2c_segment::tensor::to_rgb_f32_nchw(&frame, 640, 360);
    let mut rec: Vec<(Vec<i64>, Vec<f32>)> =
        (0..4).map(|_| (vec![1, 1, 1, 1], vec![0.0])).collect();
    for round in 0..2 {
        for i in 0..4 {
            let (mask, _, _) = engine.infer(&frame).unwrap();
            let t = |i: usize| Tensor::from_array((rec[i].0.clone(), rec[i].1.clone())).unwrap();
            let out = s
                .run(ort::inputs![
                    "src" => Tensor::from_array(([1usize, 3, 360, 640], src.clone())).unwrap(),
                    "r1i" => t(0), "r2i" => t(1), "r3i" => t(2), "r4i" => t(3),
                    "downsample_ratio" => Tensor::from_array(([1usize], vec![0.4f32])).unwrap(),
                ])
                .unwrap();
            let want = out["pha"].try_extract_tensor::<f32>().unwrap().1.to_vec();
            rec = ["r1o", "r2o", "r3o", "r4o"]
                .iter()
                .map(|n| {
                    let (sh, d) = out[*n].try_extract_tensor::<f32>().unwrap();
                    (sh.to_vec(), d.to_vec())
                })
                .collect();
            assert_eq!(mask, want, "round {round} frame {i}: masks differ");
        }
        // After a reset both start again from empty state.
        engine.reset();
        rec = (0..4).map(|_| (vec![1, 1, 1, 1], vec![0.0])).collect();
    }
}
