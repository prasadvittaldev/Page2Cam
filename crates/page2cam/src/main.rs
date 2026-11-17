mod config;
mod pattern;

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

use anyhow::Context;
use clap::Parser;
use p2c_browser::BrowserConfig;
use p2c_compositor::{FrameSink, OutputSpec, Ticker};
use p2c_core::FrameSlot;
use p2c_webcam::{list_cameras, pick_camera, WebcamCapture};
use parking_lot::RwLock;

use crate::config::Config;

#[derive(Parser, Debug)]
#[command(name = "page2cam", about = "Show a web page as a virtual webcam")]
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
        c
    }
}

fn main() -> anyhow::Result<()> {
    // CEF re-launches this executable for its helper processes; handle that first.
    if let Some(code) = p2c_browser::maybe_run_subprocess() {
        std::process::exit(code);
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    let cfg = cli.apply(Config::load(cli.config.as_deref())?);
    cfg.validate()?;

    let spec = OutputSpec {
        width: cfg.width,
        height: cfg.height,
        fps: cfg.fps,
    };
    // The virtual camera is optional: on failure keep running (preview/stream still work).
    let mut sinks: Vec<Box<dyn FrameSink>> = Vec::new();
    let mut own_device = None;
    match p2c_vcam::open_default(cfg.vcam_device.as_deref(), &spec) {
        Ok(cam) => {
            tracing::info!(
                "virtual camera: {} {}x{}@{}",
                cam.description,
                spec.width,
                spec.height,
                spec.fps
            );
            own_device = cam.own_device;
            sinks.push(cam.sink);
        }
        Err(e) => tracing::warn!("virtual camera unavailable: {e:#}"),
    }

    let page = FrameSlot::new();
    let layout = Arc::new(RwLock::new(cfg.pip_layout()?));
    let webcam = start_webcam(&cfg, own_device.as_deref());
    let webcam_slot = webcam.as_ref().map(|(slot, _)| slot.clone());

    if cli.test_pattern {
        pattern::spawn(page.clone(), cfg.width, cfg.height);
        let ticker = Ticker::spawn(spec, page, webcam_slot, layout, sinks);
        let (tx, rx) = mpsc::channel();
        ctrlc::set_handler(move || {
            let _ = tx.send(());
        })?;
        let _ = rx.recv();
        ticker.stop();
        stop_webcam(webcam);
        return Ok(());
    }

    let ticker = Ticker::spawn(spec, page.clone(), webcam_slot, layout, sinks);
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
    };
    tracing::info!(url = %cfg.url, "loading page");
    // CEF owns the main thread until shutdown; Ctrl-C asks it to close.
    let result = p2c_browser::run(browser_cfg, page, |handle| {
        if let Err(e) = ctrlc::set_handler(move || handle.shutdown()) {
            tracing::error!(error = %e, "could not install Ctrl-C handler");
        }
    });
    ticker.stop();
    stop_webcam(webcam);
    result
}

/// Start capturing the overlay webcam, never picking our own output device.
/// Any failure just means no PIP.
fn start_webcam(cfg: &Config, own_device: Option<&Path>) -> Option<(FrameSlot, WebcamCapture)> {
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
        Ok(cap) => Some((slot, cap)),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "webcam failed; running without PIP");
            None
        }
    }
}

fn stop_webcam(webcam: Option<(FrameSlot, WebcamCapture)>) {
    if let Some((_, cap)) = webcam {
        cap.stop();
    }
}
