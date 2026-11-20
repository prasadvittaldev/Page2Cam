//! Backgrounds: the full-frame and in-box choices, the generated background
//! pages Chromium shows, YouTube links, uploaded images and their thumbnails.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use p2c_compositor::{Backdrop, BoxBackdrop, FullBackdrop};
use p2c_core::Frame;

/// What fills the whole output (setting `background`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FullBg {
    /// The user's web page.
    #[default]
    Page,
    /// The uploaded full-frame image (a generated page).
    Image,
    /// A solid colour, `#rrggbb` (a generated page).
    Colour(String),
    /// A YouTube video, by its 11-character id (a generated page).
    YouTube(String),
    /// The webcam's room, blurred (drawn by the compositor).
    BlurredRoom,
}

impl FullBg {
    pub fn as_str(&self) -> &'static str {
        match self {
            FullBg::Page => "page",
            FullBg::Image => "image",
            FullBg::Colour(_) => "colour",
            FullBg::YouTube(_) => "youtube",
            FullBg::BlurredRoom => "blurred-room",
        }
    }

    /// Chromium shows a page we generate (served at `/bg/<id>/current.html`).
    pub fn is_generated(&self) -> bool {
        matches!(self, FullBg::Image | FullBg::Colour(_) | FullBg::YouTube(_))
    }

    /// R-Q1: generated pages are drawn by the compositor as the page.
    pub fn backdrop(&self) -> FullBackdrop {
        match self {
            FullBg::BlurredRoom => FullBackdrop::BlurredRoom,
            _ => FullBackdrop::Page,
        }
    }
}

/// What fills the Box style's rectangle behind the person (setting `box_background`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BoxBg {
    #[default]
    Room,
    Blur,
    Image,
    Colour(String),
}

impl BoxBg {
    pub fn as_str(&self) -> &'static str {
        match self {
            BoxBg::Room => "room",
            BoxBg::Blur => "blur",
            BoxBg::Image => "image",
            BoxBg::Colour(_) => "colour",
        }
    }

    /// The compositor's form; `image` is the decoded in-box picture.
    pub fn backdrop(&self, image: Option<&Arc<Frame>>) -> BoxBackdrop {
        match self {
            BoxBg::Room => BoxBackdrop::Room,
            BoxBg::Blur => BoxBackdrop::Blur,
            BoxBg::Image => image
                .map(|f| BoxBackdrop::Image(f.clone()))
                .unwrap_or(BoxBackdrop::Room),
            BoxBg::Colour(c) => BoxBackdrop::Colour(parse_colour(c).unwrap_or([0, 0, 0])),
        }
    }
}

/// The compositor's [`Backdrop`] for these choices.
pub fn backdrop(
    full: &FullBg,
    in_box: &BoxBg,
    image: Option<&Arc<Frame>>,
    refined: bool,
) -> Backdrop {
    Backdrop {
        full: full.backdrop(),
        in_box: in_box.backdrop(image),
        refined_edges: refined,
        hides_room: full.is_generated(),
    }
}

/// `#rrggbb` (case-insensitive) as `[r, g, b]`.
pub fn parse_colour(s: &str) -> Option<[u8; 3]> {
    let hex = s.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// A colour in its saved form (`#rrggbb`, lower case), or an error message.
pub fn normalize_colour(s: &str) -> Result<String, String> {
    let [r, g, b] =
        parse_colour(s).ok_or_else(|| format!("{:?} is not a colour (use #rrggbb)", s.trim()))?;
    Ok(format!("#{r:02x}{g:02x}{b:02x}"))
}

fn is_video_id(s: &str) -> bool {
    s.len() == 11
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The 11-character video id of a YouTube link (watch, youtu.be, shorts,
/// embed, live) or of a bare id. Timestamps and playlist parameters are
/// ignored; anything else is refused with a message for the operator.
pub fn youtube_id(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("paste a YouTube video link".into());
    }
    if is_video_id(s) {
        return Ok(s.to_string());
    }
    let not_video = || {
        Err::<String, String>(format!(
            "{s:?} is not a YouTube video link (use a link like https://www.youtube.com/watch?v=…)"
        ))
    };
    let lower = s.to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &s[8..]
    } else if lower.starts_with("http://") {
        &s[7..]
    } else if lower.contains("://") {
        return not_video();
    } else {
        s
    };
    // host / path ? query # fragment
    let rest = rest.split('#').next().unwrap_or("");
    let (host_part, path_query) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = host_part.to_ascii_lowercase();
    let host = host.split(':').next().unwrap_or("");
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("m."))
        .or_else(|| host.strip_prefix("music."))
        .unwrap_or(host);
    let (path, query) = match path_query.find('?') {
        Some(i) => (&path_query[..i], &path_query[i + 1..]),
        None => (path_query, ""),
    };
    let segments: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let checked = |id: &str| {
        if is_video_id(id) {
            Ok(id.to_string())
        } else {
            Err(format!("{s:?} has no valid YouTube video id"))
        }
    };
    match host {
        "youtu.be" => match segments.first() {
            Some(id) => checked(id),
            None => not_video(),
        },
        "youtube.com" | "youtube-nocookie.com" => match segments.as_slice() {
            ["watch"] => match query.split('&').find_map(|kv| kv.strip_prefix("v=")) {
                Some(id) => checked(id),
                None => Err(format!("{s:?} has no video id (no v=…)")),
            },
            ["shorts" | "embed" | "live" | "v", id, ..] => checked(id),
            ["playlist", ..] => {
                Err("a playlist link has no single video: open one video and copy its link".into())
            }
            _ => not_video(),
        },
        _ => not_video(),
    }
}

/// The official embed for a video id (Global Constraints).
pub fn youtube_embed_url(id: &str) -> String {
    format!(
        "https://www.youtube-nocookie.com/embed/{id}?autoplay=1&loop=1&playlist={id}&controls=0&modestbranding=1&playsinline=1"
    )
}

/// Escape for HTML text and double-quoted attributes.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The generated background page. `image_url` (relative to the page) is the
/// full-frame image for [`FullBg::Image`]. Every value is HTML-escaped; the
/// only external resource is the YouTube embed.
pub fn background_html(kind: &FullBg, image_url: Option<&str>) -> String {
    let (style, body) = match kind {
        FullBg::Colour(c) => {
            // Validated colours only (an invalid one shows black).
            let c = normalize_colour(c).unwrap_or_else(|_| "#000000".into());
            (
                format!("body{{background:{}}}", html_escape(&c)),
                String::new(),
            )
        }
        FullBg::Image => match image_url {
            Some(u) => (
                "img{display:block;width:100vw;height:100vh;object-fit:cover}".to_string(),
                format!(r#"<img src="{}" alt="">"#, html_escape(u)),
            ),
            None => (String::new(), String::new()),
        },
        FullBg::YouTube(id) => (
            "iframe{position:fixed;inset:0;width:100vw;height:100vh;border:0}".to_string(),
            format!(
                r#"<iframe src="{}" sandbox="allow-scripts allow-same-origin allow-presentation" allow="autoplay; encrypted-media; picture-in-picture" referrerpolicy="strict-origin-when-cross-origin" title="YouTube video"></iframe>"#,
                html_escape(&youtube_embed_url(id))
            ),
        ),
        FullBg::Page | FullBg::BlurredRoom => (String::new(), String::new()),
    };
    format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>Page2Cam background</title>\
         <style>html,body{{margin:0;height:100%;overflow:hidden;background:#000}}{style}</style>\
         </head><body>{body}</body></html>\n"
    )
}

/// The page to open at start-up: the saved `last_page_url` when it is a
/// user page and no `--url` was given, else `url`.
pub fn initial_page_url(cli_url_given: bool, saved: &str, url: &str) -> String {
    if !cli_url_given && is_user_page_url(saved) {
        saved.to_string()
    } else {
        url.to_string()
    }
}

/// A URL the operator's page can be restored to: http, https or file, and
/// never one of our generated background pages (`http://127.0.0.1|localhost:<port>/bg/…`).
pub fn is_user_page_url(url: &str) -> bool {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    if lower.starts_with("file://") {
        return true;
    }
    let rest = if let Some(r) = lower.strip_prefix("https://") {
        r
    } else if let Some(r) = lower.strip_prefix("http://") {
        r
    } else {
        return false;
    };
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = host.rsplit('@').next().unwrap_or(host);
    let name = match host.rfind(':') {
        Some(i) if !host.ends_with(']') => &host[..i],
        _ => host,
    };
    let local = matches!(name, "127.0.0.1" | "localhost" | "[::1]");
    !(local && path.starts_with("/bg/"))
}

/// Largest image upload, in bytes.
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
/// Uploaded images are scaled down to fit this many pixels on each side.
pub const MAX_IMAGE_SIDE: u32 = 3840;

/// Why an upload was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum ImageError {
    TooLarge,
    /// Not a PNG, JPEG or WebP file (by its content, not its name).
    NotAnImage,
    /// Looked like an image but could not be decoded.
    Bad(String),
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::TooLarge => write!(f, "the image is larger than 20 MB"),
            ImageError::NotAnImage => write!(f, "only PNG, JPEG and WebP images are accepted"),
            ImageError::Bad(why) => write!(f, "could not read the image: {why}"),
        }
    }
}

impl std::error::Error for ImageError {}

/// The image format by its first bytes.
pub fn sniff_image(bytes: &[u8]) -> Option<image::ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(image::ImageFormat::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(image::ImageFormat::Jpeg)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(image::ImageFormat::WebP)
    } else {
        None
    }
}

/// An upload, checked, scaled down and re-encoded.
pub struct PreparedImage {
    /// PNG (the picture has transparency) or JPEG.
    pub encoded: Vec<u8>,
    /// "png" | "jpg"
    pub ext: &'static str,
    pub width: u32,
    pub height: u32,
    /// Opaque BGRA, for the in-box backdrop.
    pub frame: Frame,
}

/// Check (size, real type), decode, fit into [`MAX_IMAGE_SIDE`] and
/// re-encode an uploaded image. Never returns the input bytes as they came.
pub fn prepare_image(bytes: &[u8]) -> Result<PreparedImage, ImageError> {
    prepare_image_with(bytes, MAX_IMAGE_SIDE, image_limits())
}

/// Decoder limits for uploads: at most 16384 px a side and 512 MiB.
pub fn image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(512 * 1024 * 1024);
    limits
}

/// One image decode at a time (each may take up to 512 MiB).
static DECODE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// [`prepare_image`] with a given size cap and decoder limits.
pub fn prepare_image_with(
    bytes: &[u8],
    max_side: u32,
    limits: image::Limits,
) -> Result<PreparedImage, ImageError> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(ImageError::TooLarge);
    }
    let format = sniff_image(bytes).ok_or(ImageError::NotAnImage)?;
    let _one_at_a_time = DECODE.lock();
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    reader.limits(limits);
    let img = reader
        .decode()
        .map_err(|e| ImageError::Bad(e.to_string()))?;
    let img = if img.width() > max_side || img.height() > max_side {
        img.thumbnail(max_side, max_side)
    } else {
        img
    };
    let (width, height) = (img.width(), img.height());
    let alpha = img.color().has_alpha();
    let mut encoded = Vec::new();
    let ext = if alpha {
        img.to_rgba8()
            .write_to(
                &mut std::io::Cursor::new(&mut encoded),
                image::ImageFormat::Png,
            )
            .map_err(|e| ImageError::Bad(e.to_string()))?;
        "png"
    } else {
        let rgb = img.to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 90)
            .encode_image(&rgb)
            .map_err(|e| ImageError::Bad(e.to_string()))?;
        "jpg"
    };
    let rgba = img.into_rgba8();
    let mut data = rgba.into_raw();
    for px in data.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
        px[3] = 255;
    }
    let frame =
        Frame::from_bgra(width, height, data).map_err(|e| ImageError::Bad(e.to_string()))?;
    Ok(PreparedImage {
        encoded,
        ext,
        width,
        height,
        frame,
    })
}

/// Content type for a stored image's extension.
pub fn content_type(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        _ => "image/jpeg",
    }
}

/// An uploaded picture as stored (re-encoded) in `<data dir>/backgrounds/`.
#[derive(Clone)]
pub struct StoredImage {
    pub bytes: Arc<Vec<u8>>,
    pub content_type: &'static str,
    /// Decoded, opaque BGRA (the in-box backdrop).
    pub frame: Arc<Frame>,
}

impl StoredImage {
    /// Re-check and decode a stored picture (at start-up).
    pub fn load(path: &std::path::Path) -> anyhow::Result<StoredImage> {
        let bytes = std::fs::read(path)?;
        let p = prepare_image(&bytes)?;
        Ok(StoredImage {
            content_type: content_type(p.ext),
            bytes: Arc::new(p.encoded),
            frame: Arc::new(p.frame),
        })
    }
}

/// How long a note about an automatic change (style ↔ background) shows.
pub const NOTE_TIME: Duration = Duration::from_secs(10);

/// The operator's background choices and what they need.
pub struct BgState {
    pub full: FullBg,
    pub in_box: BoxBg,
    pub refined: bool,
    /// Last colours and video chosen (offered again in the admin page).
    pub full_colour: String,
    pub box_colour: String,
    pub youtube: String,
    pub full_image: Option<StoredImage>,
    pub box_image: Option<StoredImage>,
    /// The operator's page, restored when the full frame goes back to Page.
    pub last_page_url: String,
    /// A note about an automatic change, and when it was made.
    pub note: Option<(String, Instant)>,
    /// Bumped whenever the generated page changes (`current.html?v=<n>`).
    pub version: u64,
    /// History moved since our last navigation: Back/Forward ran, or the
    /// shown URL left `load_from` (our load committed; a page-driven
    /// `history.back()` comes after that). Only then may a `/bg/` URL count
    /// as reached through history: until our own load commits (slow pages,
    /// VPN) Chromium still reports the old URL.
    pub history_moved: bool,
    /// The URL Chromium showed when we last navigated.
    pub load_from: String,
}

impl BgState {
    /// The choices saved in `cfg`. A choice that can't be honoured (missing
    /// picture, bad colour or link) falls back to Page / Room, with a
    /// warning in the returned list.
    pub fn from_config(
        cfg: &crate::config::Config,
        last_page_url: String,
    ) -> (BgState, Vec<String>) {
        let mut warn = Vec::new();
        let load = |p: &str, what: &str, warn: &mut Vec<String>| {
            if p.is_empty() {
                return None;
            }
            StoredImage::load(std::path::Path::new(p))
                .map_err(|e| warn.push(format!("{what} {p}: {e:#}")))
                .ok()
        };
        let full_image = load(&cfg.background_image, "full-frame image", &mut warn);
        let box_image = load(&cfg.box_background_image, "in-box image", &mut warn);
        let full_colour =
            normalize_colour(&cfg.background_colour).unwrap_or_else(|_| "#0b63f0".into());
        let box_colour =
            normalize_colour(&cfg.box_background_colour).unwrap_or_else(|_| "#0b63f0".into());
        let youtube = if cfg.background_youtube.trim().is_empty() {
            String::new()
        } else {
            youtube_id(&cfg.background_youtube).unwrap_or_else(|e| {
                warn.push(format!("background_youtube: {e}"));
                String::new()
            })
        };
        let full = match cfg.background.as_str() {
            "image" if full_image.is_some() => FullBg::Image,
            "colour" => match normalize_colour(&cfg.background_colour) {
                Ok(c) => FullBg::Colour(c),
                Err(e) => {
                    warn.push(format!("background_colour: {e}"));
                    FullBg::Page
                }
            },
            "youtube" if !youtube.is_empty() => FullBg::YouTube(youtube.clone()),
            "blurred-room" => FullBg::BlurredRoom,
            "page" => FullBg::Page,
            other => {
                warn.push(format!(
                    "background {other:?} can't be shown; using the page"
                ));
                FullBg::Page
            }
        };
        let in_box = match cfg.box_background.as_str() {
            "blur" => BoxBg::Blur,
            "image" if box_image.is_some() => BoxBg::Image,
            "colour" => match normalize_colour(&cfg.box_background_colour) {
                Ok(c) => BoxBg::Colour(c),
                Err(e) => {
                    warn.push(format!("box_background_colour: {e}"));
                    BoxBg::Room
                }
            },
            "room" => BoxBg::Room,
            other => {
                warn.push(format!(
                    "box_background {other:?} can't be shown; using the room"
                ));
                BoxBg::Room
            }
        };
        let st = BgState {
            full,
            in_box,
            refined: cfg.refined_edges,
            full_colour,
            box_colour,
            youtube,
            full_image,
            box_image,
            last_page_url,
            note: None,
            version: 1,
            history_moved: false,
            load_from: String::new(),
        };
        (st, warn)
    }

    /// The compositor's form of these choices.
    pub fn backdrop(&self) -> Backdrop {
        backdrop(
            &self.full,
            &self.in_box,
            self.box_image.as_ref().map(|i| &i.frame),
            self.refined,
        )
    }

    /// The note, if it is recent.
    pub fn recent_note(&self) -> Option<&str> {
        self.note
            .as_ref()
            .filter(|(_, at)| at.elapsed() < NOTE_TIME)
            .map(|(n, _)| n.as_str())
    }
}

/// Fetches the bytes at a URL (the system curl in the app; a fake in tests).
pub type Fetcher = Box<dyn Fn(&str) -> anyhow::Result<Vec<u8>> + Send + Sync>;

/// YouTube thumbnails, fetched by the engine once per video id and kept in
/// memory. A failed fetch is retried after a minute at the earliest.
pub struct ThumbCache {
    fetch: Fetcher,
    /// One slot per video, each with its own lock (held while fetching
    /// that video only).
    entries: parking_lot::Mutex<HashMap<String, Arc<parking_lot::Mutex<Option<Thumb>>>>>,
}

/// Most videos whose thumbnails are kept.
pub const THUMB_CAP: usize = 32;

enum Thumb {
    Ok(Arc<Vec<u8>>),
    Failed(Instant, String),
}

const THUMB_RETRY: Duration = Duration::from_secs(60);

impl ThumbCache {
    pub fn new(fetch: Fetcher) -> Self {
        ThumbCache {
            fetch,
            entries: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// The thumbnail URL for a video id.
    pub fn url(id: &str) -> String {
        format!("https://img.youtube.com/vi/{id}/mqdefault.jpg")
    }

    /// The JPEG thumbnail of `id` (may block on the first call for an id).
    pub fn get(&self, id: &str) -> anyhow::Result<Arc<Vec<u8>>> {
        anyhow::ensure!(is_video_id(id), "not a YouTube video id");
        let slot = {
            let mut entries = self.entries.lock();
            if !entries.contains_key(id) && entries.len() >= THUMB_CAP {
                // Full: forget one other video (not one being fetched).
                let victim = entries
                    .iter()
                    .find(|(_, s)| !s.is_locked())
                    .map(|(k, _)| k.clone());
                if let Some(k) = victim {
                    entries.remove(&k);
                }
            }
            entries.entry(id.to_string()).or_default().clone()
        };
        // Held while fetching: a second request for the same thumbnail
        // waits for the first instead of fetching it again.
        let mut entry = slot.lock();
        match &*entry {
            Some(Thumb::Ok(b)) => return Ok(b.clone()),
            Some(Thumb::Failed(at, why)) if at.elapsed() < THUMB_RETRY => {
                anyhow::bail!("{why}")
            }
            _ => {}
        }
        let got = (self.fetch)(&Self::url(id)).and_then(|b| {
            anyhow::ensure!(b.starts_with(&[0xff, 0xd8]), "the thumbnail is not a JPEG");
            Ok(b)
        });
        match got {
            Ok(b) => {
                let b = Arc::new(b);
                *entry = Some(Thumb::Ok(b.clone()));
                Ok(b)
            }
            Err(e) => {
                let why = format!("{e:#}");
                *entry = Some(Thumb::Failed(Instant::now(), why.clone()));
                anyhow::bail!("{why}")
            }
        }
    }

    /// How many videos are cached.
    #[cfg(test)]
    pub fn cached(&self) -> usize {
        self.entries.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn youtube_id_parsing_table() {
        let id = "dQw4w9WgXcQ";
        for ok in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/watch?v=dQw4w9WgXcQ&t=30s",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PL123456789",
            "https://www.youtube.com/watch?feature=share&v=dQw4w9WgXcQ",
            "http://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?t=30",
            "https://youtu.be/dQw4w9WgXcQ?si=abc&list=PL1",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ?feature=share",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ?autoplay=1",
            "https://www.youtube.com/live/dQw4w9WgXcQ?si=x",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ#t=10",
            "  https://youtu.be/dQw4w9WgXcQ  ",
            "dQw4w9WgXcQ",
        ] {
            assert_eq!(youtube_id(ok).as_deref(), Ok(id), "{ok}");
        }
        for bad in [
            "https://youtube.com/playlist?list=PLabcdefghijklmnop",
            "https://www.youtube.com/playlist?list=PL1",
            "hello",
            "",
            "   ",
            "https://vimeo.com/123456789",
            "https://evil.test/watch?v=dQw4w9WgXcQ",
            "https://youtube.com.evil.test/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=short",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ\"><x",
            "https://www.youtube.com/channel/UC1234567890",
            "https://www.youtube.com/",
            "https://youtu.be/",
            "javascript:alert(1)",
            "ftp://youtu.be/dQw4w9WgXcQ",
        ] {
            let e = youtube_id(bad).expect_err(bad);
            assert!(!e.is_empty(), "{bad}");
        }
        assert!(youtube_id("https://youtube.com/playlist?list=PL1")
            .unwrap_err()
            .contains("playlist"));
    }

    /// Every `http(s)://` in `html`.
    fn urls(html: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = html;
        while let Some(i) = rest.find("http") {
            let tail = &rest[i..];
            if tail.starts_with("http://") || tail.starts_with("https://") {
                let end = tail
                    .find(|c: char| c == '"' || c == '\'' || c == ')' || c.is_whitespace())
                    .unwrap_or(tail.len());
                out.push(tail[..end].to_string());
            }
            rest = &rest[i + 4..];
        }
        out
    }

    #[test]
    fn background_html_escapes_and_has_no_other_external_urls() {
        // Colour: solid body; a bad value never reaches the page.
        let html = background_html(&FullBg::Colour("#12AB34".into()), None);
        assert!(html.contains("background:#12ab34"), "{html}");
        let html = background_html(
            &FullBg::Colour("red}</style><script>alert(1)</script>".into()),
            None,
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(urls(&html).is_empty(), "{html}");

        // Image: object-fit cover of the given (relative) URL, escaped.
        let html = background_html(&FullBg::Image, Some("image?v=3"));
        assert!(html.contains(r#"<img src="image?v=3""#), "{html}");
        assert!(html.contains("object-fit:cover"), "{html}");
        assert!(urls(&html).is_empty(), "{html}");
        let html = background_html(&FullBg::Image, Some("x\"><script>alert(1)</script>"));
        assert!(!html.contains("<script"), "{html}");
        assert!(html.contains("x&quot;&gt;&lt;script&gt;"), "{html}");

        // YouTube: exactly the official embed, nothing else external.
        let html = background_html(&FullBg::YouTube("dQw4w9WgXcQ".into()), None);
        let found = urls(&html);
        assert_eq!(found.len(), 1, "{html}");
        assert_eq!(
            found[0].replace("&amp;", "&"),
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ?autoplay=1&loop=1&playlist=dQw4w9WgXcQ&controls=0&modestbranding=1&playsinline=1"
        );
        assert!(html.contains("<iframe"), "{html}");
        // A hostile id (never produced by `youtube_id`) is still escaped.
        let html = background_html(&FullBg::YouTube("\"><script>x".into()), None);
        assert!(!html.contains("<script"), "{html}");
        assert!(html.contains("embed/&quot;&gt;&lt;script&gt;x?"), "{html}");

        for kind in [FullBg::Page, FullBg::BlurredRoom] {
            let html = background_html(&kind, None);
            assert!(urls(&html).is_empty(), "{html}");
        }
        // No scripts, fonts or other resources in any page.
        for html in [
            background_html(&FullBg::Colour("#000000".into()), None),
            background_html(&FullBg::Image, Some("image?v=1")),
            background_html(&FullBg::YouTube("dQw4w9WgXcQ".into()), None),
        ] {
            assert!(!html.contains("<script"), "{html}");
            assert!(!html.contains("<link"), "{html}");
            assert!(!html.contains("@import"), "{html}");
        }
    }

    fn png_rgb(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([10, 200, 30]));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn image_upload_limits() {
        // Too large: refused before decoding.
        let big = vec![0u8; 25 * 1024 * 1024];
        assert_eq!(prepare_image(&big).err(), Some(ImageError::TooLarge));
        // HTML (whatever the file was called) is not an image.
        let html = b"<!doctype html><script>alert(1)</script>".to_vec();
        assert_eq!(prepare_image(&html).err(), Some(ImageError::NotAnImage));
        assert_eq!(sniff_image(&html), None);
        // A PNG header with garbage after it: refused, not stored.
        let mut fake = b"\x89PNG\r\n\x1a\n".to_vec();
        fake.extend_from_slice(&[0u8; 64]);
        assert!(matches!(prepare_image(&fake), Err(ImageError::Bad(_))));
        // Too big: scaled down to fit (same code path as 10000x10000 -> 3840,
        // which the ignored `image_upload_limits_real_10000` runs for real).
        let p = prepare_image_with(&png_rgb(600, 400), 256, image_limits()).unwrap();
        assert_eq!((p.width, p.height), (256, 171));
        assert_eq!((p.frame.width, p.frame.height), (p.width, p.height));
        // A decode that would need more memory than allowed is refused.
        let mut tight = image_limits();
        tight.max_alloc = Some(100_000);
        assert!(matches!(
            prepare_image_with(&png_rgb(600, 400), 256, tight),
            Err(ImageError::Bad(_))
        ));
        // Larger than the 16384 px side limit: refused before decoding.
        let mut hdr = png_rgb(1, 1);
        hdr[16..20].copy_from_slice(&20_000u32.to_be_bytes()); // IHDR width
        assert!(matches!(prepare_image(&hdr), Err(ImageError::Bad(_))));
        assert_eq!(image_limits().max_alloc, Some(512 * 1024 * 1024));
        let p = prepare_image(&png_rgb(64, 32)).unwrap();
        // Opaque: re-encoded as JPEG, BGRA frame opaque with the colour swapped.
        assert_eq!(p.ext, "jpg");
        assert_eq!(sniff_image(&p.encoded), Some(image::ImageFormat::Jpeg));
        let px = &p.frame.data[..4];
        assert!(px[2].abs_diff(10) < 8 && px[1].abs_diff(200) < 8 && px[0].abs_diff(30) < 8);
        assert_eq!(px[3], 255);
        // Small with transparency: PNG kept as PNG (re-encoded).
        let rgba = image::RgbaImage::from_pixel(4, 3, image::Rgba([1, 2, 3, 0]));
        let mut b = Vec::new();
        rgba.write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Png)
            .unwrap();
        let p = prepare_image(&b).unwrap();
        assert_eq!((p.ext, p.width, p.height), ("png", 4, 3));
        assert_eq!(p.frame.data[3], 255);
        // WebP is sniffed.
        assert_eq!(
            sniff_image(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(image::ImageFormat::WebP)
        );
    }

    #[test]
    #[ignore = "decodes a real 10000x10000 PNG (slow in debug builds)"]
    fn image_upload_limits_real_10000() {
        let p = prepare_image(&png_rgb(10_000, 10_000)).unwrap();
        assert_eq!((p.width, p.height), (3840, 3840));
        assert_eq!(p.ext, "jpg");
    }

    #[test]
    fn decodes_run_one_at_a_time() {
        // Two uploads at once: the second waits for the first's decode.
        let held = DECODE.lock();
        let t = std::thread::spawn(|| {
            let at = Instant::now();
            prepare_image(&png_rgb(8, 8)).unwrap();
            at.elapsed()
        });
        std::thread::sleep(Duration::from_millis(300));
        drop(held);
        assert!(t.join().unwrap() >= Duration::from_millis(250));
    }

    #[test]
    fn startup_page_url() {
        let saved = "https://saved.test/x";
        // No --url: the saved page wins over `url`.
        assert_eq!(initial_page_url(false, saved, "https://cfg.test"), saved);
        // --url given, or nothing usable saved: `url`.
        assert_eq!(
            initial_page_url(true, saved, "https://cli.test"),
            "https://cli.test"
        );
        for bad in [
            "",
            "about:blank",
            "http://127.0.0.1:8765/bg/abc/current.html",
        ] {
            assert_eq!(
                initial_page_url(false, bad, "https://cfg.test"),
                "https://cfg.test"
            );
        }
    }

    #[test]
    fn youtube_iframe_is_sandboxed() {
        let html = background_html(&FullBg::YouTube("dQw4w9WgXcQ".into()), None);
        assert!(
            html.contains(r#"sandbox="allow-scripts allow-same-origin allow-presentation""#),
            "{html}"
        );
    }

    #[test]
    fn thumbnail_fetches_lock_per_id_and_cache_is_capped() {
        // A slow fetch for one video doesn't hold up another.
        let cache = Arc::new(ThumbCache::new(Box::new(|url| {
            if url.contains("slowslowslo") {
                std::thread::sleep(Duration::from_millis(1500));
            }
            Ok(vec![0xff, 0xd8, 0xff, 0xd9])
        })));
        let c = cache.clone();
        let slow = std::thread::spawn(move || c.get("slowslowslo").unwrap());
        std::thread::sleep(Duration::from_millis(100));
        let at = Instant::now();
        cache.get("dQw4w9WgXcQ").unwrap();
        assert!(
            at.elapsed() < Duration::from_millis(700),
            "{:?}",
            at.elapsed()
        );
        slow.join().unwrap();
        // At most THUMB_CAP videos are kept.
        for i in 0..40 {
            cache.get(&format!("vid{i:08}")).unwrap();
        }
        assert!(cache.cached() <= THUMB_CAP, "{}", cache.cached());
        assert_eq!(THUMB_CAP, 32);
    }

    #[test]
    fn user_page_urls() {
        for ok in [
            "https://example.com/",
            "http://example.com/bg/x",
            "file:///C:/page.html",
            "http://127.0.0.1:9000/app",
            "HTTPS://Example.com",
        ] {
            assert!(is_user_page_url(ok), "{ok}");
        }
        for bad in [
            "http://127.0.0.1:8765/bg/0123456789abcdef0123456789abcdef/current.html",
            "http://localhost:8765/bg/abc/current.html?v=2",
            "HTTP://LOCALHOST:8765/bg/abc/image",
            "about:blank",
            "chrome://settings",
            "data:text/html,hi",
            "",
        ] {
            assert!(!is_user_page_url(bad), "{bad}");
        }
    }

    #[test]
    fn colours_parse_and_normalize() {
        assert_eq!(parse_colour("#0B63F0"), Some([0x0b, 0x63, 0xf0]));
        assert_eq!(normalize_colour(" #0B63F0 ").unwrap(), "#0b63f0");
        for bad in ["0b63f0", "#0b63f", "#0b63f00", "#gggggg", "red", ""] {
            assert!(parse_colour(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn thumbnails_are_fetched_once_per_id() {
        let calls = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let c = calls.clone();
        let cache = ThumbCache::new(Box::new(move |url| {
            c.lock().push(url.to_string());
            Ok(vec![0xff, 0xd8, 0xff, 0xd9])
        }));
        for _ in 0..3 {
            assert_eq!(
                *cache.get("dQw4w9WgXcQ").unwrap(),
                vec![0xff, 0xd8, 0xff, 0xd9]
            );
        }
        assert_eq!(
            *calls.lock(),
            vec!["https://img.youtube.com/vi/dQw4w9WgXcQ/mqdefault.jpg".to_string()]
        );
        assert!(cache.get("../../etc").is_err());
        assert_eq!(calls.lock().len(), 1);
        // A failure (or a non-JPEG answer) is not retried at once.
        let n = Arc::new(parking_lot::Mutex::new(0));
        let n2 = n.clone();
        let cache = ThumbCache::new(Box::new(move |_| {
            *n2.lock() += 1;
            Ok(b"<html>".to_vec())
        }));
        assert!(cache.get("dQw4w9WgXcQ").is_err());
        assert!(cache.get("dQw4w9WgXcQ").is_err());
        assert_eq!(*n.lock(), 1);
    }

    #[test]
    fn backdrop_mapping_follows_rq1() {
        let img = Arc::new(Frame::new_black(2, 2));
        let b = backdrop(
            &FullBg::YouTube("dQw4w9WgXcQ".into()),
            &BoxBg::Room,
            None,
            true,
        );
        assert_eq!(b.full, FullBackdrop::Page);
        assert!(b.refined_edges);
        assert_eq!(
            backdrop(&FullBg::Image, &BoxBg::Blur, None, false).full,
            FullBackdrop::Page
        );
        assert_eq!(
            backdrop(&FullBg::BlurredRoom, &BoxBg::Room, None, false).full,
            FullBackdrop::BlurredRoom
        );
        assert_eq!(
            backdrop(&FullBg::Page, &BoxBg::Colour("#ff0000".into()), None, false).in_box,
            BoxBackdrop::Colour([255, 0, 0])
        );
        assert_eq!(
            backdrop(&FullBg::Page, &BoxBg::Image, Some(&img), false).in_box,
            BoxBackdrop::Image(img.clone())
        );
        // No decoded image: the room (never a missing picture).
        assert_eq!(
            backdrop(&FullBg::Page, &BoxBg::Image, None, false).in_box,
            BoxBackdrop::Room
        );
    }

    /// I1: the generated full frames tell the compositor they hide the room.
    #[test]
    fn generated_full_frames_hide_the_room() {
        for (full, hides) in [
            (FullBg::Page, false),
            (FullBg::BlurredRoom, false),
            (FullBg::Image, true),
            (FullBg::Colour("#000000".into()), true),
            (FullBg::YouTube("dQw4w9WgXcQ".into()), true),
        ] {
            let b = backdrop(&full, &BoxBg::Room, None, false);
            assert_eq!(b.hides_room, hides, "{full:?}");
        }
    }
}
