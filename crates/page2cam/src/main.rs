mod config;
mod pattern;

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::Context;
use clap::Parser;
use p2c_browser::BrowserConfig;
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

/// Explicit config device wins; otherwise auto-detect a v4l2loopback device.
fn resolve_device(cfg: &Config, sysfs_root: &Path) -> anyhow::Result<PathBuf> {
    if let Some(d) = &cfg.vcam_device {
        return Ok(d.clone());
    }
    find_loopback_device(sysfs_root).context(
        "no virtual camera device found. Load it with:\n  \
         sudo modprobe v4l2loopback exclusive_caps=1 card_label=Page2Cam",
    )
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

    let device = resolve_device(&cfg, Path::new("/sys"))?;
    let spec = OutputSpec {
        width: cfg.width,
        height: cfg.height,
        fps: cfg.fps,
    };
    let sink = V4l2Sink::open(&device, &spec)?;
    tracing::info!(device = %device.display(), "virtual camera {}x{}@{}", spec.width, spec.height, spec.fps);

    let page = FrameSlot::new();
    let sinks: Vec<Box<dyn FrameSink>> = vec![Box::new(sink)];

    if cli.test_pattern {
        pattern::spawn(page.clone(), cfg.width, cfg.height);
        let ticker = Ticker::spawn(spec, page, sinks);
        let (tx, rx) = mpsc::channel();
        ctrlc::set_handler(move || {
            let _ = tx.send(());
        })?;
        let _ = rx.recv();
        ticker.stop();
        return Ok(());
    }

    let ticker = Ticker::spawn(spec, page.clone(), sinks);
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
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_device_error_mentions_modprobe() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("class/video4linux")).unwrap();
        let err = resolve_device(&Config::default(), root.path()).unwrap_err();
        assert!(
            format!("{err:#}").contains("modprobe v4l2loopback"),
            "{err:#}"
        );
    }

    #[test]
    fn explicit_device_wins() {
        let root = tempfile::tempdir().unwrap();
        let cfg = Config {
            vcam_device: Some("/dev/video9".into()),
            ..Config::default()
        };
        assert_eq!(
            resolve_device(&cfg, root.path()).unwrap(),
            PathBuf::from("/dev/video9")
        );
    }
}
