mod app;
mod config;
mod logging;
mod pattern;
mod update;

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, OnceLock};

use anyhow::Context;
use clap::Parser;
use p2c_admin::{AdminConfig, PreviewSink};
use p2c_audio::{MixSettings, Mixer};
use p2c_browser::BrowserConfig;
use p2c_compositor::{CpuProducer, FrameSink, OutputFormat, OutputSpec, Ticker};
use p2c_core::AudioRing;
use p2c_core::FrameSlot;
use p2c_stream::StreamController;
use p2c_webcam::{list_cameras, pick_camera, WebcamCapture};
use parking_lot::{Mutex, RwLock};

use crate::app::App;
use crate::config::Config;

#[derive(Parser, Debug)]
#[command(
    name = "page2cam",
    version,
    about = "Show a web page as a virtual webcam or live stream"
)]
struct Cli {
    /// Config file (default: <config dir>/page2cam/config.toml)
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    width: Option<u32>,
    #[arg(long)]
    height: Option<u32>,
    #[arg(long)]
    fps: Option<u32>,
    /// Virtual camera device, e.g. /dev/video5 (default: auto-detect)
    #[arg(long)]
    device: Option<PathBuf>,
    /// Webcam overlay: top-left | top-right | bottom-left | bottom-right | off
    #[arg(long)]
    pip: Option<String>,
    /// Webcam overlay width in percent of the output width (5..=50)
    #[arg(long)]
    pip_size: Option<u32>,
    /// Use the webcam whose name contains this text
    #[arg(long)]
    webcam: Option<String>,
    /// Show a moving test pattern instead of a web page
    #[arg(long)]
    test_pattern: bool,
    /// Don't open the admin page in a browser at startup
    #[arg(long)]
    no_open: bool,
    /// Admin page port (default 8765)
    #[arg(long)]
    admin_port: Option<u16>,
    /// Use a still image (PNG/JPEG) as the webcam, for testing without a camera
    #[arg(long, value_name = "IMAGE")]
    fake_webcam: Option<PathBuf>,
}

impl Cli {
    fn apply(&self, mut c: Config) -> Config {
        if let Some(v) = &self.url {
            c.url = v.clone();
        }
        if let Some(v) = self.width {
            c.width = v;
        }
        if let Some(v) = self.height {
            c.height = v;
        }
        if let Some(v) = self.fps {
            c.fps = v;
        }
        if let Some(v) = &self.device {
            c.vcam_device = Some(v.clone());
        }
        if let Some(v) = &self.pip {
            c.pip = v.clone();
        }
        if let Some(v) = self.pip_size {
            c.pip_size = v;
        }
        if let Some(v) = &self.webcam {
            c.webcam = Some(v.clone());
        }
        if let Some(v) = self.admin_port {
            c.admin_port = v;
        }
        c
    }
}

fn main() -> anyhow::Result<()> {
    // CEF re-launches this executable for its helper processes; handle that first.
    if let Some(code) = p2c_browser::maybe_run_subprocess() {
        std::process::exit(code);
    }
    logging::init();
    if let Err(e) = run() {
        tracing::error!("{e:#}");
        if cfg!(windows) {
            // Keep the console open long enough to read the error when double-clicked.
            eprintln!("\nPage2Cam stopped: {e:#}\nPress Enter to close.");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
        return Err(e);
    }
    Ok(())
}

fn run() -> anyhow::Result<()> {
    let log_file = logging::current_path();
    let cli = Cli::parse();
    let cfg = cli.apply(Config::load(cli.config.as_deref())?);
    cfg.validate()?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "Page2Cam starting {}x{}@{}",
        cfg.width,
        cfg.height,
        cfg.fps
    );
    let spec = OutputSpec {
        width: cfg.width,
        height: cfg.height,
        fps: cfg.fps,
        // Windows feeds the camera and the stream NV12 (BT.709, converted here);
        // Linux keeps BGRA (v4l2 YUYV sink, BGRA stream input).
        format: if cfg!(windows) {
            OutputFormat::Nv12
        } else {
            OutputFormat::Bgra
        },
    };

    // Outputs. The virtual camera is optional: preview and streaming still work without it.
    let mut sinks: Vec<Box<dyn FrameSink>> = Vec::new();
    let mut own_device = None;
    let mut vcam_live = p2c_vcam::CameraStatus::default();
    let vcam_status = match p2c_vcam::open_default(cfg.vcam_device.as_deref(), &spec) {
        Ok(cam) => {
            tracing::info!("virtual camera: {}", cam.description);
            own_device = cam.own_device;
            vcam_live = cam.status;
            sinks.push(cam.sink);
            format!("ok: {}", cam.description)
        }
        Err(e) => {
            tracing::warn!("virtual camera unavailable: {e:#}");
            format!("error: {e:#}")
        }
    };
    let preview = FrameSlot::new();
    sinks.push(Box::new(PreviewSink::new(
        preview.clone(),
        (cfg.fps / 12).max(1),
    )));

    let page_ring = AudioRing::new(500);
    let mic_ring = AudioRing::new(500);
    let mixer = Mixer::new(
        page_ring.clone(),
        mic_ring.clone(),
        MixSettings {
            mic_on: false,
            ..MixSettings::default()
        },
    );
    let stream = StreamController::new(mixer.clone());
    sinks.push(Box::new(stream.sink()));

    let layout = Arc::new(RwLock::new(cfg.pip_layout()?));
    let webcam = match &cli.fake_webcam {
        Some(img) => start_fake_webcam(img),
        None => start_webcam(&cfg, own_device.as_deref()),
    };
    let webcam_status = match &webcam {
        Some((_, _, name)) => format!("ok: {name}"),
        None => "none".to_string(),
    };
    let webcam_slot = webcam.as_ref().map(|(slot, _, _)| slot.clone());

    // GPU: resolved once from the `gpu` setting against a fresh adapter list
    // (adapter LUIDs are never stored: a stale one crashes Chromium).
    let adapters = p2c_d3d::enumerate();
    let (gpu_choice, gpu_note) = p2c_d3d::choose(&adapters, &p2c_d3d::GpuSetting::parse(&cfg.gpu));
    match &gpu_choice {
        p2c_d3d::GpuChoice::Gpu(a) => {
            tracing::info!(adapter = %a.name, index = a.index, "GPU chosen")
        }
        p2c_d3d::GpuChoice::Cpu { reason } => tracing::info!(%reason, "GPU not used"),
    }
    if let Some(n) = &gpu_note {
        tracing::info!("{n}");
    }
    // CPU compatibility mode keeps the 0.3.0 behaviour: the cut-out still uses
    // the high-performance GPU if there is a hardware adapter.
    let cutout_gpu = match &gpu_choice {
        p2c_d3d::GpuChoice::Gpu(a) => Some(a.index),
        p2c_d3d::GpuChoice::Cpu { .. } => {
            match p2c_d3d::choose(&adapters, &p2c_d3d::GpuSetting::Auto).0 {
                p2c_d3d::GpuChoice::Gpu(a) => Some(a.index),
                p2c_d3d::GpuChoice::Cpu { .. } => None,
            }
        }
    };

    // Cut-out: segmentation runs on its own thread and publishes the person
    // mask; the ticker combines it with the newest webcam frame.
    let matte = p2c_core::MaskSlot::new();
    let segmenter = webcam_slot
        .as_ref()
        .map(|cam| start_segmenter(&cfg, cam.clone(), matte.clone(), cutout_gpu));
    if let Some(seg) = &segmenter {
        let l = *layout.read();
        seg.set_enabled(l.visible && l.style.is_cutout());
    }
    let ticker = Ticker::spawn(
        spec,
        Box::new(CpuProducer::new(spec)),
        Some(PAGE.clone()),
        webcam_slot.clone(),
        segmenter.as_ref().map(|_| matte.clone()),
        layout.clone(),
        sinks,
    );

    let updater = Arc::new(update::Updater::new(
        env!("CARGO_PKG_VERSION"),
        update::is_installed(),
        Box::new(p2c_update::fetch_latest),
    ));
    if cfg.check_updates {
        updater.spawn_checks();
    }

    let app = Arc::new(App {
        browser: OnceLock::new(),
        layout,
        stream,
        mixer,
        mic_ring,
        mic: Mutex::new(None),
        mic_status: Mutex::new("off".into()),
        webcam_slot,
        webcam_status,
        vcam_status,
        vcam_live,
        stream_pix_fmt: match spec.format {
            OutputFormat::Nv12 => "nv12",
            OutputFormat::Bgra => "bgra",
        },
        stream_url: Mutex::new(cfg.rtmp_url.clone()),
        width: cfg.width,
        height: cfg.height,
        fps: cfg.fps,
        video_kbps: cfg.video_kbps,
        fallback_url: Mutex::new(cfg.url.clone()),
        log_file: log_file
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        segmenter,
        settings_path: config::save_path(cli.config.as_deref()),
        mic_delay_ms: Mutex::new(0),
        latencies: Mutex::new(Default::default()),
        updater: updater.clone(),
    });

    // Keep the mic in step with the cut-out's processing delay.
    let quit = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let delay_thread = {
        let (app, quit) = (app.clone(), quit.clone());
        std::thread::Builder::new()
            .name("p2c-mic-delay".into())
            .spawn(move || {
                while !quit.load(std::sync::atomic::Ordering::Relaxed) {
                    app.update_mic_delay();
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            })?
    };

    let admin = p2c_admin::serve(
        AdminConfig {
            port: cfg.admin_port,
            width: cfg.width,
            height: cfg.height,
            open_browser: cfg.open_admin && !cli.no_open,
        },
        app.clone(),
        preview,
    )?;
    tracing::info!("admin page: {}", admin.url);
    println!("\n  Page2Cam is running. Control it here:\n  {}\n  (Press Ctrl+C in this window to quit.)\n", admin.url);

    let result = if cli.test_pattern {
        pattern::spawn(PAGE.clone(), cfg.width, cfg.height);
        let (tx, rx) = mpsc::channel();
        ctrlc::set_handler(move || {
            let _ = tx.send(());
        })?;
        let _ = rx.recv();
        Ok(())
    } else {
        let profile_dir = directories::ProjectDirs::from("", "", "page2cam")
            .context("no home directory")?
            .data_dir()
            .join("cef-profile");
        let browser_cfg = BrowserConfig {
            url: cfg.url.clone(),
            width: cfg.width,
            height: cfg.height,
            fps: cfg.fps,
            profile_dir,
            audio: Some(page_ring),
        };
        tracing::info!(url = %cfg.url, "loading page");
        let app2 = app.clone();
        // CEF owns the main thread until shutdown; Ctrl-C asks it to close.
        p2c_browser::run(browser_cfg, PAGE.clone(), move |handle| {
            let h = handle.clone();
            if let Err(e) = ctrlc::set_handler(move || h.shutdown()) {
                tracing::error!(error = %e, "could not install Ctrl-C handler");
            }
            let _ = app2.browser.set(handle);
        })
    };
    tracing::info!("shutting down");
    quit.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = delay_thread.join();
    if let Some(seg) = &app.segmenter {
        seg.request_stop();
    }
    app.stream.stop();
    if let Some(m) = app.mic.lock().take() {
        m.stop();
    }
    ticker.stop();
    stop_webcam(webcam);
    result
}

/// Composited page frames. A static so the browser and ticker share one slot.
static PAGE: std::sync::LazyLock<FrameSlot> = std::sync::LazyLock::new(FrameSlot::new);

/// Start capturing the overlay webcam, never picking our own output device.
/// Any failure just means no PIP.
fn start_webcam(
    cfg: &Config,
    own_device: Option<&Path>,
) -> Option<(FrameSlot, WebcamSource, String)> {
    if !cfg.pip_layout().ok()?.visible {
        return None;
    }
    let cams = list_cameras()
        .map_err(|e| tracing::warn!(error = %e, "could not list webcams; running without PIP"))
        .ok()?;
    let Some(cam) = pick_camera(&cams, cfg.webcam.as_deref(), own_device) else {
        tracing::warn!("no webcam found; running without PIP");
        return None;
    };
    let slot = FrameSlot::new();
    match WebcamCapture::start(&cam, slot.clone()) {
        Ok(cap) => {
            tracing::info!(webcam = %cam.name, "webcam on");
            Some((slot, WebcamSource::Real(cap), cam.name.clone()))
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "webcam failed; running without PIP");
            None
        }
    }
}

/// A running webcam source.
enum WebcamSource {
    Real(WebcamCapture),
    /// `--fake-webcam`: a still image re-published at 30 fps.
    Fake(
        Arc<std::sync::atomic::AtomicBool>,
        std::thread::JoinHandle<()>,
    ),
}

fn stop_webcam(webcam: Option<(FrameSlot, WebcamSource, String)>) {
    match webcam.map(|(_, src, _)| src) {
        Some(WebcamSource::Real(cap)) => cap.stop(),
        Some(WebcamSource::Fake(stop, h)) => {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = h.join();
        }
        None => {}
    }
}

/// `--fake-webcam`: decode `img` once and publish it like a 30 fps camera.
fn start_fake_webcam(img: &Path) -> Option<(FrameSlot, WebcamSource, String)> {
    let rgba = match image::open(img) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            tracing::warn!(image = %img.display(), "fake webcam image unreadable: {e}");
            return None;
        }
    };
    let (w, h) = rgba.dimensions();
    let mut data = rgba.into_raw();
    for px in data.as_chunks_mut::<4>().0 {
        px.swap(0, 2); // RGBA -> BGRA
    }
    let frame = p2c_core::Frame::from_bgra(w, h, data).ok()?;
    let slot = FrameSlot::new();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (s2, stop2) = (slot.clone(), stop.clone());
    let handle = std::thread::spawn(move || {
        while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
            s2.put(frame.clone());
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
    });
    let name = format!("fake: {}", img.display());
    tracing::info!(webcam = %name, "webcam on");
    Some((slot, WebcamSource::Fake(stop, handle), name))
}

/// Load ONNX Runtime and start the cut-out thread (disabled until a cut-out
/// style is chosen). If the runtime can't load, the thread reports why.
fn start_segmenter(
    cfg: &Config,
    webcam: FrameSlot,
    matte: p2c_core::MaskSlot,
    gpu: Option<u32>,
) -> p2c_segment::Segmenter {
    let runtime_dir = p2c_segment::runtime_dir();
    let models = p2c_segment::models_dir();
    let loaded = p2c_segment::runtime::init(&runtime_dir).map_err(|e| format!("{e:#}"));
    match &loaded {
        Ok(()) => tracing::info!(dir = %runtime_dir.display(), "ONNX Runtime loaded"),
        Err(e) => tracing::warn!("cut-out unavailable: {e}"),
    }
    let choice = p2c_segment::EngineChoice::parse(&cfg.cutout_engine).unwrap_or_default();
    let opener: p2c_segment::Opener = Box::new(move |c| match &loaded {
        Ok(()) => p2c_segment::open_engine(&models, c, gpu),
        Err(e) => anyhow::bail!("{e}"),
    });
    p2c_segment::Segmenter::spawn(opener, webcam, matte, choice)
}
