use std::path::{Path, PathBuf};

use anyhow::Context;
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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            url: "https://example.com".into(),
            width: 1920,
            height: 1080,
            fps: 30,
            vcam_device: None,
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
        Ok(())
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
    fn load_reads_explicit_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "width = 1280\nheight = 720\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!((c.width, c.height), (1280, 720));
    }
}
