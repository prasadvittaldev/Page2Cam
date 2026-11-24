//! One overlay layer: where it sits, how it appears, and what it shows.
//! Layers travel as flat JSON objects (`{"id", "kind", "x", …, "text"}`):
//! the same shape in the admin API, the scene state and the MCP tools.
use anyhow::{anyhow, bail, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::util::{normalize_colour, youtube_id};

/// Largest `html` layer (bytes of the fragment).
pub const MAX_HTML: usize = 256 * 1024;
/// Longest title, subtitle or ticker item (characters).
pub const MAX_TEXT: usize = 500;
/// Most ticker items.
pub const MAX_ITEMS: usize = 50;
/// Longest layer name (characters).
pub const MAX_NAME: usize = 80;
/// Longest auto-hide (seconds).
pub const MAX_HIDE_AFTER_S: u32 = 3600;

/// Layer kinds.
pub const KINDS: &[&str] = &[
    "title", "ticker", "html", "template", "image", "video", "youtube",
];
/// Style presets for titles and tickers.
pub const PRESETS: &[&str] = &["news", "minimal", "brand"];
/// Enter/exit animations.
pub const ANIMS: &[&str] = &[
    "none",
    "fade",
    "slide-left",
    "slide-right",
    "slide-up",
    "slide-down",
];

/// Enter/exit animation of a layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Anim {
    None,
    #[default]
    Fade,
    SlideLeft,
    SlideRight,
    SlideUp,
    SlideDown,
}

fn default_style() -> String {
    "news".into()
}
fn default_speed() -> u32 {
    120
}
fn yes() -> bool {
    true
}

/// What a layer shows (the `kind` field picks the variant).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Content {
    /// A title or lower third: a main line and an optional second line.
    Title {
        text: String,
        #[serde(default)]
        subtitle: String,
        #[serde(default = "default_style")]
        style: String,
        /// Text colour `#rrggbb` (the preset's when absent).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        colour: Option<String>,
        /// Box colour `#rrggbb` (the preset's when absent).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<String>,
    },
    /// Items scrolling right to left in a band.
    Ticker {
        items: Vec<String>,
        /// Pixels per second (at 1920 wide).
        #[serde(default = "default_speed")]
        speed: u32,
        #[serde(default = "default_style")]
        style: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        colour: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<String>,
    },
    /// Any HTML/CSS/JS fragment, run in a sandboxed frame.
    Html { html: String },
    /// A template (built-in or the user's) with its parameters, run in a
    /// sandboxed frame; parameter changes reach it without a reload.
    Template {
        /// The template id (`list_templates`).
        template: String,
        /// The given parameters (checked against the template; the others
        /// take its defaults).
        #[serde(default)]
        params: Map<String, Value>,
    },
    /// A stored picture (PNG/JPEG/WebP/GIF; transparency kept).
    Image {
        /// Stored file name (set by the engine from an upload).
        #[serde(default)]
        asset: String,
    },
    /// A stored video file or an http(s) URL.
    Video {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asset: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default = "yes", rename = "loop")]
        looped: bool,
        #[serde(default = "yes")]
        muted: bool,
    },
    /// A YouTube video, boxed.
    Youtube {
        /// The 11-character video id.
        video: String,
        #[serde(default)]
        start_s: u32,
        #[serde(default = "yes", rename = "loop")]
        looped: bool,
        #[serde(default = "yes")]
        muted: bool,
    },
}

impl Content {
    pub fn kind(&self) -> &'static str {
        match self {
            Content::Title { .. } => "title",
            Content::Ticker { .. } => "ticker",
            Content::Html { .. } => "html",
            Content::Template { .. } => "template",
            Content::Image { .. } => "image",
            Content::Video { .. } => "video",
            Content::Youtube { .. } => "youtube",
        }
    }

    /// The stored asset file this content uses, if any.
    pub fn asset(&self) -> Option<&str> {
        match self {
            Content::Image { asset } if !asset.is_empty() => Some(asset),
            Content::Video { asset: Some(a), .. } => Some(a),
            _ => None,
        }
    }
}

/// One overlay layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub id: String,
    pub name: String,
    /// Top-left corner and size, fractions (0..1) of the output.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Stacking order (higher is in front).
    pub z: i32,
    pub visible: bool,
    /// 0..1.
    pub opacity: f32,
    pub enter: Anim,
    pub exit: Anim,
    /// Hide by itself this many seconds after being shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_after_s: Option<u32>,
    #[serde(flatten)]
    pub content: Content,
}

/// Fields every layer has (besides `id` and `kind`).
const COMMON: &[&str] = &[
    "name",
    "x",
    "y",
    "w",
    "h",
    "z",
    "visible",
    "opacity",
    "enter",
    "exit",
    "hide_after_s",
];

/// Fields of one kind, as accepted from callers (`link` for YouTube is the
/// input form of `video`; `path`/`data_base64` carry media and are handled by
/// the caller before the layer is built).
pub fn kind_fields(kind: &str) -> &'static [&'static str] {
    match kind {
        "title" => &["text", "subtitle", "style", "colour", "background"],
        "ticker" => &["items", "speed", "style", "colour", "background"],
        "html" => &["html"],
        "template" => &["template", "params"],
        "image" => &["path", "data_base64"],
        "video" => &["path", "url", "loop", "muted"],
        "youtube" => &["link", "start_s", "loop", "muted"],
        _ => &[],
    }
}

/// Default geometry `(x, y, w, h)` of a new layer of `kind` (16:9 output).
pub fn default_rect(kind: &str) -> (f32, f32, f32, f32) {
    match kind {
        "title" => (0.05, 0.72, 0.5, 0.16),
        "ticker" => (0.0, 0.92, 1.0, 0.08),
        "image" => (0.75, 0.05, 0.2, 0.2),
        // A 16:9 box on a 16:9 output has h == w.
        "video" | "youtube" => (0.6, 0.05, 0.35, 0.35),
        _ => (0.6, 0.05, 0.35, 0.35),
    }
}

fn num(v: &Value, key: &str) -> Result<f32> {
    let f = v
        .as_f64()
        .ok_or_else(|| anyhow!("{key} must be a number"))? as f32;
    ensure!(f.is_finite(), "{key} must be a finite number");
    Ok(f)
}

fn text_ok(s: &str, what: &str) -> Result<()> {
    ensure!(
        s.chars().count() <= MAX_TEXT,
        "{what} is longer than {MAX_TEXT} characters"
    );
    Ok(())
}

fn colour_opt(c: &mut Option<String>, what: &str) -> Result<()> {
    if let Some(s) = c {
        if s.trim().is_empty() {
            *c = None;
        } else {
            *c = Some(normalize_colour(s).map_err(|e| anyhow!("{what}: {e}"))?);
        }
    }
    Ok(())
}

fn style_ok(s: &str) -> Result<()> {
    ensure!(
        PRESETS.contains(&s),
        "unknown style {s:?} (use one of {})",
        PRESETS.join(", ")
    );
    Ok(())
}

/// http(s) URL check for `video.url`.
pub fn check_video_url(u: &str) -> Result<()> {
    let l = u.trim().to_ascii_lowercase();
    ensure!(
        l.starts_with("https://") || l.starts_with("http://"),
        "video url must start with http:// or https://"
    );
    ensure!(
        !u.contains(['"', '<', '>', '\\', ' ']),
        "video url contains characters that are not allowed"
    );
    Ok(())
}

impl Layer {
    /// Check every field and bring geometry into range (a layer always
    /// stays inside the output). Errors name the bad field.
    pub fn validate(&mut self) -> Result<()> {
        ensure!(
            self.name.chars().count() <= MAX_NAME,
            "name is longer than {MAX_NAME} characters"
        );
        for (k, v) in [("x", self.x), ("y", self.y), ("w", self.w), ("h", self.h)] {
            ensure!(v.is_finite(), "{k} must be a finite number");
        }
        ensure!(self.opacity.is_finite(), "opacity must be a finite number");
        self.w = self.w.clamp(0.01, 1.0);
        self.h = self.h.clamp(0.01, 1.0);
        self.x = self.x.clamp(0.0, 1.0 - self.w);
        self.y = self.y.clamp(0.0, 1.0 - self.h);
        self.opacity = self.opacity.clamp(0.0, 1.0);
        self.z = self.z.clamp(-1000, 1000);
        if let Some(s) = self.hide_after_s {
            ensure!(
                (1..=MAX_HIDE_AFTER_S).contains(&s),
                "hide_after_s must be 1..{MAX_HIDE_AFTER_S}"
            );
        }
        match &mut self.content {
            Content::Title {
                text,
                subtitle,
                style,
                colour,
                background,
            } => {
                ensure!(!text.trim().is_empty(), "a title needs text");
                text_ok(text, "text")?;
                text_ok(subtitle, "subtitle")?;
                style_ok(style)?;
                colour_opt(colour, "colour")?;
                colour_opt(background, "background")?;
            }
            Content::Ticker {
                items,
                speed,
                style,
                colour,
                background,
            } => {
                items.retain(|s| !s.trim().is_empty());
                ensure!(!items.is_empty(), "a ticker needs at least one item");
                ensure!(items.len() <= MAX_ITEMS, "at most {MAX_ITEMS} ticker items");
                for i in items.iter() {
                    text_ok(i, "a ticker item")?;
                }
                *speed = (*speed).clamp(10, 1000);
                style_ok(style)?;
                colour_opt(colour, "colour")?;
                colour_opt(background, "background")?;
            }
            Content::Html { html } => {
                ensure!(!html.trim().is_empty(), "an html layer needs html");
                ensure!(html.len() <= MAX_HTML, "html is larger than 256 KB");
            }
            Content::Template { template, params } => {
                ensure!(
                    crate::template::valid_id(template),
                    "a template layer needs template (an id from list_templates)"
                );
                let size = serde_json::to_string(params).map_or(0, |s| s.len());
                ensure!(
                    size <= crate::template::MAX_PARAMS_BYTES,
                    "params are larger than 2 MB"
                );
            }
            Content::Image { .. } => {}
            Content::Video { asset, url, .. } => {
                if let Some(u) = url.as_deref() {
                    check_video_url(u)?;
                }
                ensure!(
                    asset.is_some() || url.is_some(),
                    "a video needs a file (path) or a url"
                );
            }
            Content::Youtube { video, start_s, .. } => {
                ensure!(crate::util::is_video_id(video), "not a YouTube video id");
                *start_s = (*start_s).min(24 * 3600);
            }
        }
        Ok(())
    }

    /// A short description of the content (for lists).
    pub fn summary(&self) -> String {
        fn cut(s: &str) -> String {
            let s = s.trim().replace(['\n', '\r'], " ");
            if s.chars().count() > 60 {
                s.chars().take(57).collect::<String>() + "..."
            } else {
                s
            }
        }
        match &self.content {
            Content::Title { text, subtitle, .. } if subtitle.is_empty() => cut(text),
            Content::Title { text, subtitle, .. } => cut(&format!("{text} / {subtitle}")),
            Content::Ticker { items, .. } => {
                cut(&format!("{} items: {}", items.len(), items.join(" | ")))
            }
            Content::Html { html } => format!("{} bytes of HTML", html.len()),
            Content::Template { template, .. } => format!("template {template}"),
            Content::Image { asset } => format!("image {asset}"),
            Content::Video { asset, url, .. } => match (asset, url) {
                (Some(a), _) => format!("video file {a}"),
                (None, Some(u)) => cut(&format!("video {u}")),
                (None, None) => "video".into(),
            },
            Content::Youtube { video, .. } => format!("YouTube {video}"),
        }
    }

    /// The layer as its flat JSON object.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("a layer always serializes")
    }
}

/// Merge `patch` (a JSON object of layer fields) into `base` (a layer as
/// JSON). `id` and `kind` cannot change; unknown fields are refused;
/// `null` resets an optional field. `link` (YouTube) is turned into
/// `video`. Media fields (`path`, `data_base64`) must have been taken out
/// by the caller.
pub fn merge(base: &mut Map<String, Value>, patch: &Map<String, Value>) -> Result<()> {
    let kind = base
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    for (k, v) in patch {
        match k.as_str() {
            "id" => {
                if base.get("id") != Some(v) {
                    bail!("a layer's id cannot change (remove it and add a new one)");
                }
            }
            "kind" => {
                if v.as_str() != Some(kind.as_str()) {
                    bail!("a layer's kind cannot change (remove it and add a new one)");
                }
            }
            "link" if kind == "youtube" => {
                let link = v.as_str().ok_or_else(|| anyhow!("link must be text"))?;
                let id = youtube_id(link).map_err(|e| anyhow!(e))?;
                base.insert("video".into(), Value::String(id));
            }
            "path" | "data_base64" => {
                bail!("{k} is handled before merging (internal error)")
            }
            k if COMMON.contains(&k) || kind_fields(&kind).contains(&k) => {
                if matches!(k, "x" | "y" | "w" | "h" | "opacity") {
                    num(v, k)?;
                }
                if v.is_null() {
                    base.remove(k);
                } else {
                    base.insert(k.to_string(), v.clone());
                }
            }
            k => bail!(
                "unknown field {k:?} for a {kind} layer (fields: {}, {})",
                COMMON.join(", "),
                kind_fields(&kind).join(", ")
            ),
        }
    }
    Ok(())
}

/// Build a layer from `base` JSON and validate it; errors name the field.
pub fn from_json(v: Value) -> Result<Layer> {
    let mut l: Layer = serde_json::from_value(v).map_err(|e| anyhow!("bad layer: {e}"))?;
    l.validate()?;
    Ok(l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn title() -> Layer {
        from_json(json!({
            "id": "t", "name": "T", "kind": "title", "text": "Hello",
            "x": 0.1, "y": 0.7, "w": 0.5, "h": 0.2, "z": 0, "visible": false,
            "opacity": 1.0, "enter": "fade", "exit": "slide-left"
        }))
        .unwrap()
    }

    #[test]
    fn layer_json_is_flat_and_round_trips() {
        let l = title();
        let v = l.to_json();
        assert_eq!(v["kind"], "title");
        assert_eq!(v["text"], "Hello");
        assert_eq!(v["exit"], "slide-left");
        assert_eq!(v["style"], "news");
        assert!(v.get("hide_after_s").is_none());
        assert_eq!(from_json(v).unwrap(), l);
        let y = from_json(json!({
            "id": "y", "name": "", "kind": "youtube", "video": "dQw4w9WgXcQ",
            "x": 0.0, "y": 0.0, "w": 0.3, "h": 0.3, "z": 1, "visible": true,
            "opacity": 1.0, "enter": "none", "exit": "none"
        }))
        .unwrap();
        assert_eq!(y.to_json()["loop"], true);
        assert_eq!(y.to_json()["muted"], true);
    }

    #[test]
    fn geometry_is_kept_inside_the_output() {
        let mut l = title();
        (l.x, l.y, l.w, l.h, l.opacity) = (0.9, -1.0, 0.5, 3.0, 7.0);
        l.validate().unwrap();
        assert_eq!((l.x, l.y, l.w, l.h, l.opacity), (0.5, 0.0, 0.5, 1.0, 1.0));
        l.x = f32::NAN;
        assert!(l.validate().is_err());
    }

    #[test]
    fn content_is_checked() {
        let bad = |patch: Value| {
            let mut base = title().to_json().as_object().unwrap().clone();
            merge(&mut base, patch.as_object().unwrap())
                .and_then(|()| from_json(Value::Object(base)))
                .unwrap_err()
                .to_string()
        };
        assert!(bad(json!({"text": "  "})).contains("needs text"));
        assert!(bad(json!({"text": "x".repeat(501)})).contains("longer than 500"));
        assert!(bad(json!({"style": "fancy"})).contains("unknown style"));
        assert!(bad(json!({"colour": "red"})).contains("colour"));
        assert!(bad(json!({"items": ["a"]})).contains("unknown field \"items\""));
        assert!(bad(json!({"kind": "ticker"})).contains("kind cannot change"));
        assert!(bad(json!({"id": "other"})).contains("id cannot change"));
        assert!(bad(json!({"x": "left"})).contains("x must be a number"));
        assert!(bad(json!({"hide_after_s": 0})).contains("hide_after_s"));
        assert!(bad(json!({"enter": "spin"})).contains("bad layer"));
    }

    #[test]
    fn merge_is_partial_and_null_resets() {
        let mut base = title().to_json().as_object().unwrap().clone();
        merge(
            &mut base,
            json!({"subtitle": "World", "colour": "#FFFFFF", "hide_after_s": 5})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        let l = from_json(Value::Object(base.clone())).unwrap();
        assert_eq!(l.x, 0.1, "untouched fields stay");
        assert_eq!(l.hide_after_s, Some(5));
        match &l.content {
            Content::Title {
                text,
                subtitle,
                colour,
                ..
            } => {
                assert_eq!((text.as_str(), subtitle.as_str()), ("Hello", "World"));
                assert_eq!(colour.as_deref(), Some("#ffffff"));
            }
            c => panic!("{c:?}"),
        }
        merge(
            &mut base,
            json!({"colour": null, "hide_after_s": null})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        let l = from_json(Value::Object(base)).unwrap();
        assert_eq!(l.hide_after_s, None);
        assert!(matches!(l.content, Content::Title { colour: None, .. }));
    }

    #[test]
    fn html_limit_and_youtube_link() {
        let mut h = json!({
            "id": "h", "name": "", "kind": "html", "html": "<b>x</b>",
            "x": 0.0, "y": 0.0, "w": 0.3, "h": 0.3, "z": 1, "visible": true,
            "opacity": 1.0, "enter": "none", "exit": "none"
        });
        assert!(from_json(h.clone()).is_ok());
        h["html"] = Value::String("a".repeat(MAX_HTML + 1));
        assert!(from_json(h).unwrap_err().to_string().contains("256 KB"));
        let mut y = json!({
            "id": "y", "name": "", "kind": "youtube", "video": "dQw4w9WgXcQ",
            "x": 0.0, "y": 0.0, "w": 0.3, "h": 0.3, "z": 1, "visible": true,
            "opacity": 1.0, "enter": "none", "exit": "none"
        })
        .as_object()
        .unwrap()
        .clone();
        merge(
            &mut y,
            json!({"link": "https://youtu.be/aqz-KE-bpKQ?t=3"})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(y["video"], "aqz-KE-bpKQ");
        assert!(merge(&mut y, json!({"link": "nope"}).as_object().unwrap()).is_err());
    }

    #[test]
    fn video_needs_a_source_and_a_web_url() {
        let v = |extra: Value| {
            let mut j = json!({
                "id": "v", "name": "", "kind": "video",
                "x": 0.0, "y": 0.0, "w": 0.3, "h": 0.3, "z": 1, "visible": true,
                "opacity": 1.0, "enter": "none", "exit": "none"
            });
            for (k, val) in extra.as_object().unwrap() {
                j[k] = val.clone();
            }
            from_json(j)
        };
        assert!(v(json!({})).is_err());
        assert!(v(json!({"url": "file:///c:/x.mp4"})).is_err());
        assert!(v(json!({"url": "javascript:alert(1)"})).is_err());
        assert!(v(json!({"url": "https://example.com/a.mp4"})).is_ok());
        assert!(v(json!({"asset": "v-1.mp4"})).is_ok());
    }
}
