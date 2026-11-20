//! The tools: names, schemas, argument checks and their [`BackendCall`].

use base64::Engine as _;
use serde_json::{json, Map, Value};

/// Largest page an agent can show (`show_page`).
pub const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
/// Largest background picture (same as the engine's upload limit).
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_TEXT_CHARS: usize = 10_000;
const MAX_URL_BYTES: usize = 8 * 1024;

/// `navigate` actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Back,
    Forward,
    Reload,
}

impl Nav {
    pub fn as_str(self) -> &'static str {
        match self {
            Nav::Back => "back",
            Nav::Forward => "forward",
            Nav::Reload => "reload",
        }
    }
}

/// One checked request to Page2Cam. Bodies marked "partial" hold only the
/// fields the agent gave; the backend merges them with the current values.
#[derive(Clone, Debug, PartialEq)]
pub enum BackendCall {
    Status,
    Start,
    Stop,
    ForceRestart,
    /// The URL as given (the engine normalises and checks the scheme).
    OpenUrl(String),
    Navigate(Nav),
    ShowPage {
        name: String,
        html: String,
    },
    ListPages,
    /// The engine's `POST /api/input` body (output coordinates 0..1).
    Input(Value),
    Snapshot,
    GetOverlay,
    /// Partial `PipDto` (`visible`, `x`, `y`, `width`, `style` in engine form).
    SetOverlay(Value),
    /// "quality" | "fast"
    SetCutoutQuality(String),
    GetCrop,
    /// `{x, y, w, h}` in 0..1 of the camera picture.
    SetCrop(Value),
    ResetCrop,
    /// To the saved destination with the saved key.
    StartStream,
    StopStream,
    /// Partial `AudioSettings`.
    SetAudio(Value),
    ListGpus,
    SetGpu(String),
    InstallUpdate,
    GetBackground,
    /// Partial `BackgroundDto`.
    SetBackground(Value),
    /// `row` = "full" | "box"; bytes are the picture file.
    UploadBackgroundImage {
        row: String,
        bytes: Vec<u8>,
    },
}

impl BackendCall {
    /// The call changes something (recorded in the activity list).
    pub fn changes(&self) -> bool {
        !matches!(
            self,
            BackendCall::Status
                | BackendCall::ListPages
                | BackendCall::Snapshot
                | BackendCall::GetOverlay
                | BackendCall::GetCrop
                | BackendCall::ListGpus
                | BackendCall::GetBackground
        )
    }

    /// Short text for the activity list (no page HTML, no picture bytes).
    pub fn summary(&self) -> String {
        match self {
            BackendCall::OpenUrl(u) => format!("open {}", crate::redact(u)),
            BackendCall::Navigate(n) => format!("navigate {}", n.as_str()),
            BackendCall::ShowPage { name, html } => {
                format!("show page {name:?} ({} KB)", html.len().div_ceil(1024))
            }
            BackendCall::Input(v) => format!("input {}", v["kind"].as_str().unwrap_or("?")),
            BackendCall::SetOverlay(v) => format!("set overlay {v}"),
            BackendCall::SetCutoutQuality(q) => format!("cut-out {q}"),
            BackendCall::SetCrop(v) => format!("set crop {v}"),
            BackendCall::SetAudio(v) => format!("set audio {v}"),
            BackendCall::SetGpu(g) => format!("set GPU {g:?}"),
            BackendCall::SetBackground(v) => {
                format!("set background {}", crate::redact(&v.to_string()))
            }
            BackendCall::UploadBackgroundImage { row, bytes } => {
                format!(
                    "upload {row} background ({} KB)",
                    bytes.len().div_ceil(1024)
                )
            }
            other => format!("{other:?}").to_lowercase(),
        }
    }
}

/// What a backend answers.
#[derive(Clone, Debug, PartialEq)]
pub enum BackendReply {
    Json(Value),
    /// A JPEG picture.
    Image(Vec<u8>),
    Done(String),
}

/// Carries out checked calls (the tray: its own controls or the engine's API).
pub trait Backend {
    /// `agent` is the MCP client's name (for the activity list and balloons).
    /// Errors are plain-English messages for the agent.
    fn call(&self, agent: &str, req: BackendCall) -> Result<BackendReply, String>;
}

const CANVAS: &str = "The output is a 1920x1080 picture (16:9). Coordinates are 0..1 of it (0,0 top left). \
The webcam overlay is drawn on top: style box = a rectangle whose top-left moves with x,y inside a 2% margin, \
width = fraction of the output width (0.05-0.5), height from the webcam's aspect; style cutout_small = the person \
cut out, standing on the bottom edge at x, about half the height; style cutout_full = the person cut out, full \
height, standing at the bottom at x. Call get_overlay to see where the person is.";

/// All tools, as `tools/list` returns them.
pub fn tool_defs() -> Vec<Value> {
    let none = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    let unit = json!({ "type": "number", "minimum": 0, "maximum": 1 });
    let colour = json!({ "type": "string", "pattern": "^#[0-9a-fA-F]{6}$" });
    let t = |name: &str, desc: String, schema: &Value| json!({ "name": name, "description": desc, "inputSchema": schema });
    vec![
        t("status", "Page2Cam's state: running or stopped, health, page, webcam, overlay, cut-out, background, stream, audio, GPU, update. Works while stopped.".into(), &none),
        t("start", "Start Page2Cam (the video, the virtual camera picture and the control page). Takes up to 30 s.".into(), &none),
        t("stop", "Stop Page2Cam. Ends a running stream; call apps then see the 'stopped' picture.".into(), &none),
        t("force_restart", "Kill and restart Page2Cam and re-register the virtual camera. Use when it is stuck.".into(), &none),
        t("open_url", "Show a web page (http, https or file URL) as the background of the camera picture.".into(),
            &json!({ "type": "object", "required": ["url"], "additionalProperties": false,
                "properties": { "url": { "type": "string", "maxLength": MAX_URL_BYTES } } })),
        t("navigate", "Back, forward or reload in the page.".into(),
            &json!({ "type": "object", "required": ["action"], "additionalProperties": false,
                "properties": { "action": { "enum": ["back", "forward", "reload"] } } })),
        t("show_page", format!("Save a single-file HTML page as Documents\\Page2Cam\\pages\\<name>.html (overwriting one with that name) and show it. \
Inline images as data: URIs or link http(s) assets. Design for 1920x1080 and leave the area where the person stands clear. {CANVAS}"),
            &json!({ "type": "object", "required": ["name", "html"], "additionalProperties": false,
                "properties": { "name": { "type": "string", "maxLength": 64 },
                    "html": { "type": "string", "maxLength": MAX_HTML_BYTES } } })),
        t("list_pages", "The pages saved with show_page. Works while stopped.".into(), &none),
        t("click", format!("Click the page at x,y (0..1 of the output). {CANVAS}"),
            &json!({ "type": "object", "required": ["x", "y"], "additionalProperties": false,
                "properties": { "x": unit, "y": unit } })),
        t("scroll", "Scroll the page at x,y (0..1) by dx,dy pixels (positive dy scrolls down).".into(),
            &json!({ "type": "object", "required": ["x", "y", "dy"], "additionalProperties": false,
                "properties": { "x": unit, "y": unit, "dx": { "type": "number" }, "dy": { "type": "number" } } })),
        t("type_text", "Type text into the focused field of the page (click it first).".into(),
            &json!({ "type": "object", "required": ["text"], "additionalProperties": false,
                "properties": { "text": { "type": "string", "maxLength": MAX_TEXT_CHARS } } })),
        t("press_key", "Press a key in the page: enter, tab, escape, backspace, delete, space, left, up, right, down, home, end, pageup, pagedown, f1-f12, or one character.".into(),
            &json!({ "type": "object", "required": ["key"], "additionalProperties": false,
                "properties": { "key": { "type": "string", "maxLength": 32 } } })),
        t("snapshot", format!("The picture the camera sends now, as a JPEG (1280x720). Use it to check your work. {CANVAS}"), &none),
        t("get_overlay", "Where the webcam overlay is and its style.".into(), &none),
        t("set_overlay", format!("Move, resize, show/hide or restyle the webcam overlay. Give only what changes. \
Choosing box while a full-frame background other than page is on returns the background to page. {CANVAS}"),
            &json!({ "type": "object", "additionalProperties": false, "minProperties": 1,
                "properties": { "visible": { "type": "boolean" }, "x": unit, "y": unit,
                    "width": { "type": "number", "minimum": 0.05, "maximum": 0.5 },
                    "style": { "enum": ["box", "cutout_small", "cutout_full"] } } })),
        t("set_cutout_quality", "Cut-out model: quality (smoother edges, more GPU) or fast (light).".into(),
            &json!({ "type": "object", "required": ["quality"], "additionalProperties": false,
                "properties": { "quality": { "enum": ["quality", "fast"] } } })),
        t("get_crop", "The webcam crop: a rectangle in 0..1 of the camera picture.".into(), &none),
        t("set_crop", "Crop the webcam picture to x,y,w,h (0..1 of the camera picture; at least 0.1 wide and high).".into(),
            &json!({ "type": "object", "required": ["x", "y", "w", "h"], "additionalProperties": false,
                "properties": { "x": unit, "y": unit, "w": unit, "h": unit } })),
        t("reset_crop", "Use the whole webcam picture again.".into(), &none),
        t("start_stream", "Start streaming (e.g. YouTube Live) to the destination and key saved in the control page. You cannot pass or read the key.".into(), &none),
        t("stop_stream", "Stop the stream.".into(), &none),
        t("set_audio", "Stream audio: microphone and page sound on/off and gain (0-4, 1 = unchanged). Give only what changes. Audio goes to the stream only.".into(),
            &json!({ "type": "object", "additionalProperties": false, "minProperties": 1,
                "properties": { "mic_on": { "type": "boolean" }, "mic_gain": { "type": "number", "minimum": 0, "maximum": 4 },
                    "page_on": { "type": "boolean" }, "page_gain": { "type": "number", "minimum": 0, "maximum": 4 } } })),
        t("list_gpus", "The choices for 'Video processing': auto, each GPU, cpu; and the one in use.".into(), &none),
        t("set_gpu", "Choose what makes the video (a name from list_gpus). Page2Cam restarts to apply it.".into(),
            &json!({ "type": "object", "required": ["choice"], "additionalProperties": false,
                "properties": { "choice": { "type": "string", "maxLength": 200 } } })),
        t("install_update", "Download, verify and install the newer Page2Cam version on offer (status shows it). Page2Cam restarts.".into(), &none),
        t("get_background", "The background choices: full frame, in box, colours, YouTube video, refined edges, uploaded pictures.".into(), &none),
        t("set_background", "Choose what is behind the person. Give only what changes. \
full = full-frame background: page (the web page), image (the uploaded picture), colour, youtube (a video link, plays full screen looping with sound on the stream only), blurred-room (the webcam's room, blurred). \
Any full other than page switches the overlay style to cutout_full. \
in_box = behind the person inside the box (box style): room, blur, image, colour. \
refined_edges = smoother hair and shoulder edges (a little more GPU/CPU).".into(),
            &json!({ "type": "object", "additionalProperties": false, "minProperties": 1,
                "properties": {
                    "full": { "enum": ["page", "image", "colour", "youtube", "blurred-room"] },
                    "full_colour": colour, "youtube": { "type": "string", "maxLength": 2048 },
                    "in_box": { "enum": ["room", "blur", "image", "colour"] },
                    "box_colour": colour, "refined_edges": { "type": "boolean" } } })),
        t("upload_background_image", "Upload the picture for a background row (full = full frame, box = in box) and show it there. \
Give a local file path or base64 file bytes (PNG, JPEG or WebP, up to 20 MB).".into(),
            &json!({ "type": "object", "required": ["row"], "additionalProperties": false,
                "properties": { "row": { "enum": ["full", "box"] },
                    "path": { "type": "string" }, "base64": { "type": "string" } } })),
    ]
}

/// The tool names, in `tools/list` order.
pub fn tool_names() -> Vec<String> {
    tool_defs()
        .iter()
        .filter_map(|t| t["name"].as_str().map(String::from))
        .collect()
}

type Args = Map<String, Value>;

fn only(args: &Args, allowed: &[&str]) -> Result<(), String> {
    match args.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("unknown argument {k:?}")),
        None => Ok(()),
    }
}

fn str_arg<'a>(args: &'a Args, k: &str) -> Result<Option<&'a str>, String> {
    match args.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("{k} must be text")),
    }
}

fn req_str<'a>(args: &'a Args, k: &str) -> Result<&'a str, String> {
    str_arg(args, k)?.ok_or_else(|| format!("{k} is required"))
}

fn num_arg(args: &Args, k: &str, lo: f64, hi: f64) -> Result<Option<f64>, String> {
    match args.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() && (lo..=hi).contains(&n) => Ok(Some(n)),
            _ => Err(format!("{k} must be a number from {lo} to {hi}")),
        },
    }
}

fn req_num(args: &Args, k: &str, lo: f64, hi: f64) -> Result<f64, String> {
    num_arg(args, k, lo, hi)?.ok_or_else(|| format!("{k} is required"))
}

fn bool_arg(args: &Args, k: &str) -> Result<Option<bool>, String> {
    match args.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{k} must be true or false")),
    }
}

fn one_of<'a>(k: &str, v: &'a str, allowed: &[&str]) -> Result<&'a str, String> {
    if allowed.contains(&v) {
        Ok(v)
    } else {
        Err(format!("{k} must be one of {}", allowed.join(", ")))
    }
}

fn colour_arg(args: &Args, k: &str) -> Result<Option<String>, String> {
    match str_arg(args, k)? {
        None => Ok(None),
        Some(c)
            if c.len() == 7
                && c.starts_with('#')
                && c[1..].chars().all(|h| h.is_ascii_hexdigit()) =>
        {
            Ok(Some(c.to_ascii_lowercase()))
        }
        Some(_) => Err(format!("{k} must be a colour like #1a2b3c")),
    }
}

fn nonempty(m: Map<String, Value>) -> Result<Value, String> {
    if m.is_empty() {
        Err("give at least one setting to change".into())
    } else {
        Ok(Value::Object(m))
    }
}

/// Checks a `tools/call` and builds its [`BackendCall`]. `Err(None)` means
/// the tool doesn't exist; `Err(Some(msg))` is a bad-argument tool error.
pub(crate) fn parse_call(name: &str, args: &Args) -> Result<BackendCall, Option<String>> {
    use BackendCall as C;
    let simple = |c: BackendCall| -> Result<BackendCall, Option<String>> {
        only(args, &[]).map(|()| c).map_err(Some)
    };
    let r: Result<BackendCall, String> = match name {
        "status" => return simple(C::Status),
        "start" => return simple(C::Start),
        "stop" => return simple(C::Stop),
        "force_restart" => return simple(C::ForceRestart),
        "list_pages" => return simple(C::ListPages),
        "snapshot" => return simple(C::Snapshot),
        "get_overlay" => return simple(C::GetOverlay),
        "get_crop" => return simple(C::GetCrop),
        "reset_crop" => return simple(C::ResetCrop),
        "start_stream" => return simple(C::StartStream),
        "stop_stream" => return simple(C::StopStream),
        "list_gpus" => return simple(C::ListGpus),
        "install_update" => return simple(C::InstallUpdate),
        "get_background" => return simple(C::GetBackground),
        "open_url" => (|| {
            only(args, &["url"])?;
            let u = req_str(args, "url")?.trim();
            if u.is_empty() || u.len() > MAX_URL_BYTES {
                return Err("url must be 1 to 8192 characters".into());
            }
            Ok(C::OpenUrl(u.to_string()))
        })(),
        "navigate" => (|| {
            only(args, &["action"])?;
            Ok(C::Navigate(match req_str(args, "action")? {
                "back" => Nav::Back,
                "forward" => Nav::Forward,
                "reload" => Nav::Reload,
                _ => return Err("action must be back, forward or reload".into()),
            }))
        })(),
        "show_page" => (|| {
            only(args, &["name", "html"])?;
            let name = req_str(args, "name")?;
            let html = req_str(args, "html")?;
            if name.trim().is_empty() {
                return Err("name is required".into());
            }
            if html.trim().is_empty() {
                return Err("html is required".into());
            }
            if html.len() > MAX_HTML_BYTES {
                return Err(
                    "html is larger than 2 MB; link big images instead of inlining them".into(),
                );
            }
            Ok(C::ShowPage {
                name: name.to_string(),
                html: html.to_string(),
            })
        })(),
        "click" => (|| {
            only(args, &["x", "y"])?;
            let (x, y) = (req_num(args, "x", 0.0, 1.0)?, req_num(args, "y", 0.0, 1.0)?);
            Ok(C::Input(json!({ "kind": "click", "x": x, "y": y })))
        })(),
        "scroll" => (|| {
            only(args, &["x", "y", "dx", "dy"])?;
            let (x, y) = (req_num(args, "x", 0.0, 1.0)?, req_num(args, "y", 0.0, 1.0)?);
            let dx = num_arg(args, "dx", -10_000.0, 10_000.0)?.unwrap_or(0.0);
            let dy = req_num(args, "dy", -10_000.0, 10_000.0)?;
            Ok(C::Input(
                json!({ "kind": "scroll", "x": x, "y": y, "dx": dx, "dy": dy }),
            ))
        })(),
        "type_text" => (|| {
            only(args, &["text"])?;
            let text = req_str(args, "text")?;
            let n = text.chars().count();
            if n == 0 || n > MAX_TEXT_CHARS {
                return Err("text must be 1 to 10000 characters".into());
            }
            Ok(C::Input(json!({ "kind": "text", "text": text })))
        })(),
        "press_key" => (|| {
            only(args, &["key"])?;
            let key = req_str(args, "key")?;
            if key.is_empty() || key.chars().count() > 32 {
                return Err("key must be a key name or one character".into());
            }
            Ok(C::Input(json!({ "kind": "key", "key": key })))
        })(),
        "set_overlay" => (|| {
            only(args, &["visible", "x", "y", "width", "style"])?;
            let mut m = Map::new();
            if let Some(v) = bool_arg(args, "visible")? {
                m.insert("visible".into(), v.into());
            }
            for k in ["x", "y"] {
                if let Some(v) = num_arg(args, k, 0.0, 1.0)? {
                    m.insert(k.into(), v.into());
                }
            }
            if let Some(v) = num_arg(args, "width", 0.05, 0.5)? {
                m.insert("width".into(), v.into());
            }
            if let Some(s) = str_arg(args, "style")? {
                let s = match s.replace('-', "_").as_str() {
                    "box" => "box",
                    "cutout_small" => "cutout-small",
                    "cutout_full" => "cutout-full",
                    _ => return Err("style must be one of box, cutout_small, cutout_full".into()),
                };
                m.insert("style".into(), s.into());
            }
            nonempty(m).map(C::SetOverlay)
        })(),
        "set_cutout_quality" => (|| {
            only(args, &["quality"])?;
            let q = one_of("quality", req_str(args, "quality")?, &["quality", "fast"])?;
            Ok(C::SetCutoutQuality(q.into()))
        })(),
        "set_crop" => (|| {
            only(args, &["x", "y", "w", "h"])?;
            let mut m = Map::new();
            for k in ["x", "y", "w", "h"] {
                m.insert(k.into(), req_num(args, k, 0.0, 1.0)?.into());
            }
            Ok(C::SetCrop(Value::Object(m)))
        })(),
        "set_audio" => (|| {
            only(args, &["mic_on", "mic_gain", "page_on", "page_gain"])?;
            let mut m = Map::new();
            for k in ["mic_on", "page_on"] {
                if let Some(v) = bool_arg(args, k)? {
                    m.insert(k.into(), v.into());
                }
            }
            for k in ["mic_gain", "page_gain"] {
                if let Some(v) = num_arg(args, k, 0.0, 4.0)? {
                    m.insert(k.into(), v.into());
                }
            }
            nonempty(m).map(C::SetAudio)
        })(),
        "set_gpu" => (|| {
            only(args, &["choice"])?;
            let c = req_str(args, "choice")?.trim();
            if c.is_empty() || c.len() > 200 {
                return Err("choice must be a name from list_gpus".into());
            }
            Ok(C::SetGpu(c.into()))
        })(),
        "set_background" => (|| {
            only(
                args,
                &[
                    "full",
                    "full_colour",
                    "youtube",
                    "in_box",
                    "box_colour",
                    "refined_edges",
                ],
            )?;
            let mut m = Map::new();
            if let Some(f) = str_arg(args, "full")? {
                let f = one_of(
                    "full",
                    f,
                    &["page", "image", "colour", "youtube", "blurred-room"],
                )?;
                m.insert("full".into(), f.into());
            }
            if let Some(c) = colour_arg(args, "full_colour")? {
                m.insert("full_colour".into(), c.into());
            }
            if let Some(y) = str_arg(args, "youtube")? {
                let y = y.trim();
                if y.is_empty() || y.len() > 2048 {
                    return Err("youtube must be a YouTube link or video id".into());
                }
                m.insert("full_youtube".into(), y.into());
            }
            if let Some(b) = str_arg(args, "in_box")? {
                let b = one_of("in_box", b, &["room", "blur", "image", "colour"])?;
                m.insert("in_box".into(), b.into());
            }
            if let Some(c) = colour_arg(args, "box_colour")? {
                m.insert("box_colour".into(), c.into());
            }
            if let Some(r) = bool_arg(args, "refined_edges")? {
                m.insert("refined_edges".into(), r.into());
            }
            nonempty(m).map(C::SetBackground)
        })(),
        "upload_background_image" => (|| {
            only(args, &["row", "path", "base64"])?;
            let row = one_of("row", req_str(args, "row")?, &["full", "box"])?.to_string();
            let bytes = match (str_arg(args, "path")?, str_arg(args, "base64")?) {
                (Some(p), None) => read_picture(p)?,
                (None, Some(b)) => {
                    // 4 base64 chars per 3 bytes: refuse before decoding.
                    if b.len() / 4 * 3 > MAX_IMAGE_BYTES + 3 {
                        return Err("the picture is larger than 20 MB".into());
                    }
                    let clean: String = b.chars().filter(|c| !c.is_ascii_whitespace()).collect();
                    base64::engine::general_purpose::STANDARD
                        .decode(clean)
                        .map_err(|_| "base64 is not valid base64".to_string())?
                }
                _ => return Err("give either path or base64".into()),
            };
            if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
                return Err("the picture must be 1 byte to 20 MB".into());
            }
            Ok(C::UploadBackgroundImage { row, bytes })
        })(),
        _ => return Err(None),
    };
    r.map_err(Some)
}

fn read_picture(path: &str) -> Result<Vec<u8>, String> {
    let p = std::path::Path::new(path);
    let meta = std::fs::metadata(p).map_err(|e| format!("cannot read {path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path} is not a file"));
    }
    if meta.len() > MAX_IMAGE_BYTES as u64 {
        return Err("the picture is larger than 20 MB".into());
    }
    std::fs::read(p).map_err(|e| format!("cannot read {path}: {e}"))
}
