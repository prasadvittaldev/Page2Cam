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
        assert!(outs.iter().any(|o| o == n), "RVM output {n} missing: {outs:?}");
    }

    println!("MediaPipe selfie:");
    let (mp, _) = runtime::session(&models.join("selfie_landscape.onnx"), false).unwrap();
    let (ins, outs) = describe(&mp);
    assert_eq!(ins.len(), 1);
    assert_eq!(outs.len(), 1);
}
