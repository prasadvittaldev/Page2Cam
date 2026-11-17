mod app;
mod config;
mod logging;
mod pattern;

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, OnceLock};

use anyhow::Context;
use clap::Parser;
use p2c_admin::{AdminConfig, PreviewSink};
use p2c_audio::{MixSettings, Mixer};
use p2c_browser::BrowserConfig;
use p2c_compositor::{FrameSink, OutputSpec, Ticker};
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
    };

    // Outputs. The virtual camera is optional: preview and streaming still work without it.
    let mut sinks: Vec<Box<dyn FrameSink>> = Vec::new();
    let mut own_device = None;
    let vcam_status = match p2c_vcam::open_default(cfg.vcam_device.as_deref(), &spec) {
        Ok(cam) => {
            tracing::info!("virtual camera: {}", cam.description);
            own_device = cam.own_device;
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
    let webcam = start_webcam(&cfg, own_device.as_deref());
    let webcam_status = match &webcam {
        Some((_, _, name)) => format!("ok: {name}"),
        None => "none".to_string(),
    };
    let webcam_slot = webcam.as_ref().map(|(slot, _, _)| slot.clone());
    let ticker = Ticker::spawn(
        spec,
        PAGE.clone(),
        webcam_slot.clone(),
        layout.clone(),
        sinks,
    );

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
        stream_url: Mutex::new(cfg.rtmp_url.clone()),
        width: cfg.width,
        height: cfg.height,
        fps: cfg.fps,
        video_kbps: cfg.video_kbps,
        fallback_url: Mutex::new(cfg.url.clone()),
        log_file: log_file
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    });

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
) -> Option<(FrameSlot, WebcamCapture, String)> {
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
            Some((slot, cap, cam.name.clone()))
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "webcam failed; running without PIP");
            None
        }
    }
}

fn stop_webcam(webcam: Option<(FrameSlot, WebcamCapture, String)>) {
    if let Some((_, cap, _)) = webcam {
        cap.stop();
    }
}
