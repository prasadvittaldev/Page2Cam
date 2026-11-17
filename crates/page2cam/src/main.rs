mod config;
mod pattern;

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::Context;
use clap::Parser;
use p2c_compositor::{FrameSink, OutputSpec, Ticker};
use p2c_core::FrameSlot;
use p2c_vcam::{find_loopback_device, V4l2Sink};

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
        c
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    let cfg = cli.apply(Config::load(cli.config.as_deref())?);
    cfg.validate()?;

    let device = cfg
        .vcam_device
        .clone()
        .or_else(|| find_loopback_device(Path::new("/sys")))
        .context("no v4l2loopback device found")?;
    let spec = OutputSpec {
        width: cfg.width,
        height: cfg.height,
        fps: cfg.fps,
    };
    let sink = V4l2Sink::open(&device, &spec)?;
    tracing::info!(device = %device.display(), "virtual camera {}x{}@{}", spec.width, spec.height, spec.fps);

    let page = FrameSlot::new();
    anyhow::ensure!(
        cli.test_pattern,
        "web pages are not wired up yet; use --test-pattern"
    );
    pattern::spawn(page.clone(), cfg.width, cfg.height);
    let sinks: Vec<Box<dyn FrameSink>> = vec![Box::new(sink)];
    let ticker = Ticker::spawn(spec, page, sinks);

    let (tx, rx) = mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })?;
    let _ = rx.recv();
    ticker.stop();
    Ok(())
}
