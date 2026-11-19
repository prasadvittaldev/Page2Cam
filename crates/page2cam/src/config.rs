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
    /// Admin page port on 127.0.0.1 (another free port is used if taken).
    pub admin_port: u16,
    /// Open the admin page in the default browser at startup.
    pub open_admin: bool,
    /// RTMP server pre-filled in the admin page.
    pub rtmp_url: String,
    /// Stream video bitrate in kbit/s.
    pub video_kbps: u32,
    /// Webcam overlay style: box | cutout-small | cutout-full
    pub overlay_style: String,
    /// Cut-out engine: quality | fast
    pub cutout_engine: String,
    /// Look for new versions on GitHub (a small request every few hours).
    pub check_updates: bool,
    /// GPU for the cut-out and video: auto | cpu | exact adapter name.
    pub gpu: String,
    /// Webcam crop as [x, y, width, height], fractions of the camera frame.
    /// A list of the wrong length or with NaN means no crop.
    pub webcam_crop: Vec<f32>,
    /// Full-frame background: page | image | colour | youtube | blurred-room
    pub background: String,
    /// The full-frame picture (re-encoded copy in the data folder).
    pub background_image: String,
    /// Full-frame colour, `#rrggbb`.
    pub background_colour: String,
    /// Full-frame YouTube video (link or 11-character id).
    pub background_youtube: String,
    /// In-box background (Box style): room | blur | image | colour
    pub box_background: String,
    pub box_background_image: String,
    pub box_background_colour: String,
    /// Guided-filter cut-out edges.
    pub refined_edges: bool,
    /// The operator's page, restored when the full-frame background goes back to Page.
    pub last_page_url: String,
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
            admin_port: 8765,
            open_admin: true,
            rtmp_url: "rtmp://a.rtmp.youtube.com/live2".into(),
            video_kbps: 6000,
            overlay_style: "box".into(),
            cutout_engine: "quality".into(),
            check_updates: true,
            gpu: "auto".into(),
            webcam_crop: vec![0.0, 0.0, 1.0, 1.0],
            background: "page".into(),
            background_image: String::new(),
            background_colour: "#0b63f0".into(),
            background_youtube: String::new(),
            box_background: "room".into(),
            box_background_image: String::new(),
            box_background_colour: "#0b63f0".into(),
            refined_edges: false,
            last_page_url: String::new(),
        }
    }
}

impl Config {
    /// Load from `path`, else `<config_dir>/page2cam/config.toml` if present, else defaults.
    /// Load from `path`, else `page2cam.toml` next to the executable, else
    /// `<config_dir>/page2cam/config.toml`, else defaults.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Config> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let user_dir = directories::ProjectDirs::from("", "", "page2cam")
            .map(|d| d.config_dir().to_path_buf());
        let cfg = match config_path(path, exe_dir.as_deref(), user_dir.as_deref()) {
            Some(p) => {
                let text = std::fs::read_to_string(&p)
                    .with_context(|| format!("reading {}", p.display()))?;
                tracing::info!(config = %p.display(), "using settings file");
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
        anyhow::ensure!(
            p2c_compositor::OverlayStyle::parse(&self.overlay_style).is_some(),
            "unknown overlay_style {:?} (use box, cutout-small or cutout-full)",
            self.overlay_style
        );
        anyhow::ensure!(
            ["quality", "fast"].contains(&self.cutout_engine.as_str()),
            "unknown cutout_engine {:?} (use quality or fast)",
            self.cutout_engine
        );
        anyhow::ensure!(
            (500..=20_000).contains(&self.video_kbps),
            "video_kbps must be 500..=20000 (got {})",
            self.video_kbps
        );
        anyhow::ensure!(
            ["page", "image", "colour", "youtube", "blurred-room"]
                .contains(&self.background.as_str()),
            "unknown background {:?} (use page, image, colour, youtube or blurred-room)",
            self.background
        );
        anyhow::ensure!(
            ["room", "blur", "image", "colour"].contains(&self.box_background.as_str()),
            "unknown box_background {:?} (use room, blur, image or colour)",
            self.box_background
        );
        Ok(())
    }

    /// The webcam crop from `webcam_crop`, made valid (see [`p2c_core::Crop::clamp`]).
    pub fn crop(&self) -> p2c_core::Crop {
        match self.webcam_crop[..] {
            [x, y, w, h] => p2c_core::Crop { x, y, w, h }.clamp(),
            _ => p2c_core::Crop::FULL,
        }
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
        let mut l = PipLayout::preset(corner, width);
        l.style = p2c_compositor::OverlayStyle::parse(&self.overlay_style).unwrap_or_default();
        if l.style == p2c_compositor::OverlayStyle::CutoutFull {
            l.x = 0.5;
        }
        Ok(l)
    }
}

/// Which settings file to use (see [`Config::load`]).
fn config_path(
    explicit: Option<&Path>,
    exe_dir: Option<&Path>,
    user_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    let beside = exe_dir
        .map(|d| d.join("page2cam.toml"))
        .filter(|p| p.is_file());
    beside.or_else(|| {
        user_dir
            .map(|d| d.join("config.toml"))
            .filter(|p| p.is_file())
    })
}

/// Set one top-level `key = "value"` in the TOML file at `path`, keeping its
/// comments and other settings. Creates the file (and folder) if missing.
pub fn save_choice(path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    edit_settings(path, |doc| doc[key] = toml_edit::value(value))
}

/// Set several top-level keys at once (one write), keeping comments.
pub fn save_values(path: &Path, values: &[(&str, toml_edit::Value)]) -> anyhow::Result<()> {
    edit_settings(path, |doc| {
        for (k, v) in values {
            doc[*k] = toml_edit::value(v.clone());
        }
    })
}

/// Save the webcam crop as `webcam_crop = [x, y, w, h]`, keeping comments.
pub fn save_crop(path: &Path, crop: p2c_core::Crop) -> anyhow::Result<()> {
    let mut arr = toml_edit::Array::new();
    for v in [crop.x, crop.y, crop.w, crop.h] {
        // f32 -> f64 would print 0.10000000149; round-trip through the short decimal form.
        arr.push(v.to_string().parse::<f64>().unwrap_or(0.0));
    }
    edit_settings(path, |doc| doc["webcam_crop"] = toml_edit::value(arr))
}

/// Serializes settings writes: the admin page saves from several threads,
/// and two read-modify-writes of the same file would lose one change.
static SETTINGS_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Read-modify-write the settings file under `SETTINGS_WRITE`, keeping its
/// comments. Creates the file (and folder) if missing.
fn edit_settings(
    path: &Path,
    edit: impl FnOnce(&mut toml_edit::DocumentMut),
) -> anyhow::Result<()> {
    let _guard = SETTINGS_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    edit(&mut doc);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))
}

/// Where operator choices are saved: the settings file in use, else the
/// user's config file.
pub fn save_path(explicit: Option<&Path>) -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let user_dir =
        directories::ProjectDirs::from("", "", "page2cam").map(|d| d.config_dir().to_path_buf());
    config_path(explicit, exe_dir.as_deref(), user_dir.as_deref())
        .or_else(|| user_dir.map(|d| d.join("config.toml")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_choice_keeps_comments() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("page2cam.toml");
        std::fs::write(&p, "# my comment\nfps = 25\noverlay_style = \"box\"\n").unwrap();
        save_choice(&p, "overlay_style", "cutout-full").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# my comment"), "{text}");
        let c: Config = toml::from_str(&text).unwrap();
        assert_eq!((c.overlay_style.as_str(), c.fps), ("cutout-full", 25));
    }

    #[test]
    fn settings_roundtrip_keeps_comments() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("page2cam.toml");
        std::fs::write(&p, "# my comment\nfps = 25 # inline\n").unwrap();
        // Defaults when the keys are missing.
        let c: Config = toml::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(c.background, "page");
        assert_eq!(c.box_background, "room");
        assert!(!c.refined_edges);
        assert_eq!(c.last_page_url, "");
        save_values(
            &p,
            &[
                ("background", "youtube".into()),
                ("background_youtube", "dQw4w9WgXcQ".into()),
                ("background_colour", "#0b63f0".into()),
                ("background_image", r"C:\data\backgrounds\full.jpg".into()),
                ("box_background", "colour".into()),
                ("box_background_colour", "#112233".into()),
                ("box_background_image", "/data/backgrounds/box.png".into()),
                ("refined_edges", true.into()),
                ("last_page_url", "https://example.com/a?b=1".into()),
            ],
        )
        .unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# my comment"), "{text}");
        assert!(text.contains("# inline"), "{text}");
        let c: Config = toml::from_str(&text).unwrap();
        assert!(c.validate().is_ok());
        assert_eq!(c.fps, 25);
        assert_eq!(c.background, "youtube");
        assert_eq!(c.background_youtube, "dQw4w9WgXcQ");
        assert_eq!(c.background_colour, "#0b63f0");
        assert_eq!(c.background_image, r"C:\data\backgrounds\full.jpg");
        assert_eq!(c.box_background, "colour");
        assert_eq!(c.box_background_colour, "#112233");
        assert_eq!(c.box_background_image, "/data/backgrounds/box.png");
        assert!(c.refined_edges);
        assert_eq!(c.last_page_url, "https://example.com/a?b=1");
        // Unknown background names are refused like other choices.
        for (k, v) in [("background", "aquarium"), ("box_background", "fog")] {
            let c: Config = toml::from_str(&format!("{k} = \"{v}\"")).unwrap();
            assert!(c.validate().is_err(), "{k}");
        }
    }

    #[test]
    fn save_choice_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("config.toml");
        save_choice(&p, "cutout_engine", "fast").unwrap();
        let c: Config = toml::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(c.cutout_engine, "fast");
    }

    #[test]
    fn rejects_bad_overlay_style() {
        let c = Config {
            overlay_style: "sparkles".into(),
            ..Config::default()
        };
        assert!(c.validate().is_err());
        let c = Config {
            cutout_engine: "turbo".into(),
            ..Config::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn crop_defaults_to_full() {
        let c = Config::default();
        assert_eq!(c.webcam_crop, vec![0.0, 0.0, 1.0, 1.0]);
        assert!(c.crop().is_full());
    }

    #[test]
    fn config_crop_bad_values_are_clamped_or_default() {
        let c: Config = toml::from_str("webcam_crop = [0.95, 0.0, 0.5, 0.01]").unwrap();
        let k = c.crop();
        assert_eq!((k.x, k.w, k.h), (0.5, 0.5, 0.10));
        let c: Config = toml::from_str("webcam_crop = [0.1, 0.2]").unwrap();
        assert!(c.crop().is_full());
        let c = Config {
            webcam_crop: vec![f32::NAN, 0.0, 1.0, 1.0],
            ..Config::default()
        };
        assert!(c.crop().is_full());
    }

    #[test]
    fn save_crop_keeps_comments() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("page2cam.toml");
        std::fs::write(&p, "# my comment\nfps = 25\n").unwrap();
        let k = p2c_core::Crop {
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.5,
        };
        save_crop(&p, k).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# my comment"), "{text}");
        let c: Config = toml::from_str(&text).unwrap();
        assert_eq!((c.fps, c.crop()), (25, k));
    }

    #[test]
    fn concurrent_saves_lose_nothing() {
        // The admin page saves from several threads (style, engine, crop...).
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("page2cam.toml");
        std::fs::write(&p, "# keep\n").unwrap();
        let k = p2c_core::Crop {
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.5,
        };
        std::thread::scope(|s| {
            for t in 0..8 {
                let p = &p;
                s.spawn(move || {
                    for n in 0..25 {
                        if t == 0 {
                            save_crop(p, k).unwrap();
                        } else {
                            save_choice(p, &format!("k{t}"), &n.to_string()).unwrap();
                        }
                    }
                });
            }
        });
        let text = std::fs::read_to_string(&p).unwrap();
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        for t in 1..8 {
            assert_eq!(doc[&format!("k{t}")].as_str(), Some("24"), "{text}");
        }
        assert!(doc.contains_key("webcam_crop"), "{text}");
        assert!(text.contains("# keep"), "{text}");
    }

    #[test]
    fn gpu_defaults_to_auto() {
        let c = Config::default();
        assert_eq!(c.gpu, "auto");
        assert!(c.validate().is_ok());
        let c: Config = toml::from_str("gpu = \"NVIDIA RTX 2000 Ada\"").unwrap();
        assert!(c.validate().is_ok());
    }

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
    fn stream_defaults_target_youtube() {
        let c = Config::default();
        assert_eq!(c.rtmp_url, "rtmp://a.rtmp.youtube.com/live2");
        assert_eq!(
            (c.video_kbps, c.admin_port, c.open_admin),
            // YouTube's minimum for 1080p30 H.264 is 5000 kbps.
            (6000, 8765, true)
        );
        let c = Config {
            video_kbps: 100,
            ..Config::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn config_next_to_exe_wins_over_user_dir() {
        let exe_dir = tempfile::tempdir().unwrap();
        let user_dir = tempfile::tempdir().unwrap();
        std::fs::write(user_dir.path().join("config.toml"), "fps = 25").unwrap();
        assert_eq!(
            config_path(None, Some(exe_dir.path()), Some(user_dir.path())),
            Some(user_dir.path().join("config.toml"))
        );
        std::fs::write(exe_dir.path().join("page2cam.toml"), "fps = 24").unwrap();
        assert_eq!(
            config_path(None, Some(exe_dir.path()), Some(user_dir.path())),
            Some(exe_dir.path().join("page2cam.toml"))
        );
        let explicit = Path::new("/x/y.toml");
        assert_eq!(
            config_path(Some(explicit), Some(exe_dir.path()), None),
            Some(explicit.to_path_buf())
        );
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
