use std::path::{Path, PathBuf};

use anyhow::Context;
use p2c_compositor::{Corner, PipLayout};
use serde::Deserialize;

/// App settings, loaded from TOML and overridden by CLI flags.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub vcam_device: Option<PathBuf>,
    /// Webcam overlay position: top-left | top-right | bottom-left | bottom-right | off
    pub pip: String,
    /// Webcam overlay width as a percent of the output width (5..=50).
    pub pip_size: u32,
    /// Pick the webcam whose name contains this (case-insensitive); default: first one.
    pub webcam: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            url: "https://example.com".into(),
            width: 1920,
            height: 1080,
            fps: 30,
            vcam_device: None,
            pip: "bottom-right".into(),
            pip_size: 25,
            webcam: None,
        }
    }
}

impl Config {
    /// Load from `path`, else `<config_dir>/page2cam/config.toml` if present, else defaults.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Config> {
        let default_path = directories::ProjectDirs::from("", "", "page2cam")
            .map(|d| d.config_dir().join("config.toml"));
        let path = match path {
            Some(p) => Some(p.to_path_buf()),
            None => default_path.filter(|p| p.exists()),
        };
        let cfg = match path {
            Some(p) => {
                let text = std::fs::read_to_string(&p)
                    .with_context(|| format!("reading {}", p.display()))?;
                toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))?
            }
            None => Config::default(),
        };
        Ok(cfg)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.width.is_multiple_of(2) && self.height.is_multiple_of(2),
            "width and height must be even (got {}x{})",
            self.width,
            self.height
        );
        anyhow::ensure!(
            (320..=3840).contains(&self.width),
            "width must be 320..=3840 (got {})",
            self.width
        );
        anyhow::ensure!(
            (240..=2160).contains(&self.height),
            "height must be 240..=2160 (got {})",
            self.height
        );
        anyhow::ensure!(
            (1..=60).contains(&self.fps),
            "fps must be 1..=60 (got {})",
            self.fps
        );
        anyhow::ensure!(
            (5..=50).contains(&self.pip_size),
            "pip size must be 5..=50 percent (got {})",
            self.pip_size
        );
        self.pip_layout()?;
        Ok(())
    }

    /// The webcam overlay layout described by `pip` and `pip_size`.
    pub fn pip_layout(&self) -> anyhow::Result<PipLayout> {
        let width = self.pip_size as f32 / 100.0;
        let corner = match self.pip.as_str() {
            "top-left" => Corner::TopLeft,
            "top-right" => Corner::TopRight,
            "bottom-left" => Corner::BottomLeft,
            "bottom-right" => Corner::BottomRight,
            "off" => {
                return Ok(PipLayout {
                    visible: false,
                    ..PipLayout::preset(Corner::BottomRight, width)
                })
            }
            other => anyhow::bail!(
                "unknown pip position {other:?} (use top-left, top-right, bottom-left, bottom-right or off)"
            ),
        };
        Ok(PipLayout::preset(corner, width))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_1080p30() {
        let c = Config::default();
        assert_eq!((c.width, c.height, c.fps), (1920, 1080, 30));
        assert_eq!(c.url, "https://example.com");
        assert!(c.validate().is_ok());
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let c: Config = toml::from_str("fps = 60").unwrap();
        assert_eq!((c.fps, c.width), (60, 1920));
    }

    #[test]
    fn rejects_odd_dimensions() {
        let c = Config {
            width: 1281,
            ..Config::default()
        };
        assert!(c.validate().is_err());
        let c = Config {
            height: 721,
            ..Config::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_fps_zero() {
        let c = Config {
            fps: 0,
            ..Config::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_pip_size_out_of_range() {
        let c = Config {
            pip_size: 80,
            ..Config::default()
        };
        assert!(c.validate().is_err());
        let c = Config {
            pip_size: 4,
            ..Config::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn pip_defaults_bottom_right_25() {
        let l = Config::default().pip_layout().unwrap();
        assert_eq!(l, PipLayout::preset(Corner::BottomRight, 0.25));
    }

    #[test]
    fn pip_off_is_hidden_and_bad_corner_rejected() {
        let c = Config {
            pip: "off".into(),
            ..Config::default()
        };
        assert!(!c.pip_layout().unwrap().visible);
        let c = Config {
            pip: "top-left".into(),
            pip_size: 10,
            ..Config::default()
        };
        assert_eq!(
            c.pip_layout().unwrap(),
            PipLayout::preset(Corner::TopLeft, 0.10)
        );
        let c = Config {
            pip: "middle".into(),
            ..Config::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn load_reads_explicit_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "width = 1280\nheight = 720\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!((c.width, c.height), (1280, 720));
    }
}
