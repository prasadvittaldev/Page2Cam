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
    /// The monitors for the screen background.
    ListScreens,
    GetBackground,
    /// Partial `BackgroundDto`.
    SetBackground(Value),
    /// `row` = "full" | "box"; bytes are the picture file.
    UploadBackgroundImage {
        row: String,
        bytes: Vec<u8>,
    },
    ListLayers,
    /// The engine's `POST /api/layers` body (`kind` plus layer fields).
    AddLayer(Value),
    /// Partial layer fields for `POST /api/layers/<id>`.
    UpdateLayer {
        id: String,
        patch: Value,
    },
    ShowLayer(String),
    HideLayer(String),
    RemoveLayer(String),
    ClearLayers,
    /// Back to front.
    ReorderLayers(Vec<String>),
    /// Answered by the server itself (no backend call).
    Guide(Option<String>),
    /// Built-in and user templates, optionally of one category.
    ListTemplates(Option<String>),
    GetTemplate {
        id: String,
        include_html: bool,
    },
    /// The engine's `POST /api/templates` body.
    SaveTemplate(Value),
    DeleteTemplate(String),
    ListScenes,
    SaveScene {
        name: String,
        include_background: bool,
        include_webcam: bool,
    },
    LoadScene {
        id: String,
        replace: bool,
    },
    DeleteScene(String),
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
                | BackendCall::ListScreens
                | BackendCall::ListLayers
                | BackendCall::Guide(_)
                | BackendCall::ListTemplates(_)
                | BackendCall::GetTemplate { .. }
                | BackendCall::ListScenes
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
            BackendCall::AddLayer(v) => format!(
                "add {} layer{}",
                v["kind"].as_str().unwrap_or("?"),
                v["id"]
                    .as_str()
                    .map(|i| format!(" {i:?}"))
                    .unwrap_or_default()
            ),
            BackendCall::UpdateLayer { id, patch } => {
                let keys: Vec<&str> = patch
                    .as_object()
                    .map(|m| m.keys().map(String::as_str).collect())
                    .unwrap_or_default();
                format!("update layer {id:?} ({})", keys.join(", "))
            }
            BackendCall::ShowLayer(id) => format!("show layer {id:?}"),
            BackendCall::HideLayer(id) => format!("hide layer {id:?}"),
            BackendCall::RemoveLayer(id) => format!("remove layer {id:?}"),
            BackendCall::ClearLayers => "hide all layers".into(),
            BackendCall::ReorderLayers(ids) => format!("reorder layers {}", ids.join(", ")),
            BackendCall::SaveTemplate(v) => format!(
                "save template {:?}",
                v["id"].as_str().or(v["from_layer"].as_str()).unwrap_or("?")
            ),
            BackendCall::DeleteTemplate(id) => format!("delete template {id:?}"),
            BackendCall::SaveScene { name, .. } => format!("save scene {name:?}"),
            BackendCall::LoadScene { id, replace } => format!(
                "load scene {id:?} ({})",
                if *replace { "replace" } else { "add" }
            ),
            BackendCall::DeleteScene(id) => format!("delete scene {id:?}"),
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
full = full-frame background: page (the web page), image (the uploaded picture), colour, youtube (a video link, plays full screen looping with sound on the stream only), screen (a whole monitor, live: everything on it is shown to the call or stream; screen = an id from list_screens, default the primary monitor), blurred-room (the webcam's room, blurred). \
Any full other than page switches the overlay style to cutout_full. \
in_box = behind the person inside the box (box style): room, blur, image, colour. \
refined_edges = smoother hair and shoulder edges (a little more GPU/CPU).".into(),
            &json!({ "type": "object", "additionalProperties": false, "minProperties": 1,
                "properties": {
                    "full": { "enum": ["page", "image", "colour", "youtube", "screen", "blurred-room"] },
                    "full_colour": colour, "youtube": { "type": "string", "maxLength": 2048 },
                    "screen": { "type": "string", "maxLength": 64 },
                    "in_box": { "enum": ["room", "blur", "image", "colour"] },
                    "box_colour": colour, "refined_edges": { "type": "boolean" } } })),
        t("list_screens", "The monitors the screen background can show: id, index, name, resolution, primary. Fails where screen capture is not available (Windows only).".into(), &none),
        t("upload_background_image", "Upload the picture for a background row (full = full frame, box = in box) and show it there. \
Give a local file path or base64 file bytes (PNG, JPEG or WebP, up to 20 MB).".into(),
            &json!({ "type": "object", "required": ["row"], "additionalProperties": false,
                "properties": { "row": { "enum": ["full", "box"] },
                    "path": { "type": "string" }, "base64": { "type": "string" } } })),
        t("list_layers", "The overlay layers (graphics drawn over everything, the person included), back to front: id, name, kind, x, y, w, h, z, visible, opacity, animations, auto-hide and a short summary of the content.".into(), &none),
        t("add_layer", format!("Add an overlay layer drawn over the whole picture, the person included; it shows at once (enter animation) unless visible is false, in front of the others. Returns its id. {LAYERS}"),
            &layer_schema(true)),
        t("update_layer", format!("Change an overlay layer: give its id and only what changes (a ticker keeps scrolling, a video keeps playing when you only move it). Use null to clear hide_after_s or a colour. {LAYERS}"),
            &layer_schema(false)),
        t("show_layer", "Show an overlay layer (plays its enter animation and restarts its auto-hide).".into(),
            &json!({ "type": "object", "required": ["id"], "additionalProperties": false,
                "properties": { "id": { "type": "string", "maxLength": 40 } } })),
        t("hide_layer", "Hide an overlay layer (plays its exit animation; it stays in the list).".into(),
            &json!({ "type": "object", "required": ["id"], "additionalProperties": false,
                "properties": { "id": { "type": "string", "maxLength": 40 } } })),
        t("remove_layer", "Delete an overlay layer (and its stored picture or video).".into(),
            &json!({ "type": "object", "required": ["id"], "additionalProperties": false,
                "properties": { "id": { "type": "string", "maxLength": 40 } } })),
        t("clear_layers", "Hide every overlay layer (they stay in the list).".into(), &none),
        t("reorder_layers", "Restack overlay layers: ids from back to front (the last is in front). Layers not listed stay behind them.".into(),
            &json!({ "type": "object", "required": ["ids"], "additionalProperties": false,
                "properties": { "ids": { "type": "array", "minItems": 1, "maxItems": 32,
                    "items": { "type": "string", "maxLength": 40 } } } })),
        t("guide", "How to run a good show for the user: call it first for a use case. No topic = the index. topic = the user's role or goal (\"doctor\", \"webinar\", \"release demo\", \"teacher\": a playbook with background, webcam, graphics, steps and rules), a template id (its parameters and example calls), \"catalogue\" (every template), or a how-to (templates, scenes, layout, privacy, writing-templates). Works while stopped.".into(),
            &json!({ "type": "object", "additionalProperties": false,
                "properties": { "topic": { "type": "string", "maxLength": 200 } } })),
        t("list_templates", "Overlay templates (built-in and the user's own): id, name, category, one-line description. category: basics, time, cards, lists, data, cta, calm, software, mine. Add one with add_layer kind template.".into(),
            &json!({ "type": "object", "additionalProperties": false,
                "properties": { "category": { "type": "string", "maxLength": 40 } } })),
        t("get_template", "One template in full: params with types, defaults and help, an example, a live-change example. include_html adds its HTML (always for the user's own).".into(),
            &json!({ "type": "object", "required": ["id"], "additionalProperties": false,
                "properties": { "id": { "type": "string", "maxLength": 40 }, "include_html": { "type": "boolean" } } })),
        t("save_template", "Save a template of the user's own (replaces one with the same id). Either a full definition (id, name, html, params, example, rect, category, description), or from_layer = an html layer's id (plus name), or copy_of = a template id plus id and the fields to change. See guide \"writing-templates\".".into(),
            &json!({ "type": "object", "additionalProperties": false,
                "properties": {
                    "id": { "type": "string", "maxLength": 40 }, "name": { "type": "string", "maxLength": 80 },
                    "category": { "type": "string", "maxLength": 40 }, "description": { "type": "string", "maxLength": 500 },
                    "html": { "type": "string", "maxLength": MAX_LAYER_HTML },
                    "params": { "type": "array", "maxItems": 40, "items": { "type": "object" } },
                    "example": { "type": "object" }, "update": { "type": "object" },
                    "rect": { "type": "array", "minItems": 4, "maxItems": 4, "items": { "type": "number", "minimum": 0, "maximum": 1 } },
                    "full_frame": { "type": "boolean" },
                    "from_layer": { "type": "string", "maxLength": 40 }, "copy_of": { "type": "string", "maxLength": 40 } } })),
        t("delete_template", "Delete a template of the user's own (built-ins stay). Layers using it show \"template missing\".".into(),
            &json!({ "type": "object", "required": ["id"], "additionalProperties": false,
                "properties": { "id": { "type": "string", "maxLength": 40 } } })),
        t("list_scenes", "Saved looks (scenes): id, name, number of layers, whether background and webcam are included.".into(), &none),
        t("save_scene", "Save the current overlay layers (shown or hidden) as a look under a name, optionally with the background and the webcam overlay. The same name replaces it.".into(),
            &json!({ "type": "object", "required": ["name"], "additionalProperties": false,
                "properties": { "name": { "type": "string", "maxLength": 80 },
                    "include_background": { "type": "boolean" }, "include_webcam": { "type": "boolean" } } })),
        t("load_scene", "Bring back a saved look: mode replace (default: the other layers are removed first) or add (in front of the current ones). Layers show as saved; a saved background and webcam overlay come back too. Says what was skipped.".into(),
            &json!({ "type": "object", "required": ["id"], "additionalProperties": false,
                "properties": { "id": { "type": "string", "maxLength": 40 }, "mode": { "enum": ["replace", "add"] } } })),
        t("delete_scene", "Delete a saved look.".into(),
            &json!({ "type": "object", "required": ["id"], "additionalProperties": false,
                "properties": { "id": { "type": "string", "maxLength": 40 } } })),
    ]
}

/// What agents need to know about overlay layers.
const LAYERS: &str = "x, y = top-left corner, w, h = size, all 0..1 of the 1920x1080 output (defaults per kind: \
title 0.05,0.72 0.5x0.16; ticker 0,0.92 1x0.08; html/video/youtube 0.6,0.05 0.35x0.35 (a 16:9 box has h = w); image 0.75,0.05 0.2x0.2). \
Kinds: title (text, optional subtitle: a title or lower third), ticker (items scrolling right to left, speed px/s), \
html (any HTML/CSS/JS fragment with its own <style>/<script>, up to 256 KB, run in a sandboxed frame on a transparent \
background: infographics, charts, small animations; it may load from the web but cannot reach Page2Cam), \
image (path to a local PNG/JPEG/WebP/GIF or data_base64; transparency kept), video (path to a local MP4/WebM up to 200 MB, or an http(s) url; \
loop and muted default true), youtube (link; muted and loop default true). Styles for title and ticker: news (navy band, red accent), \
minimal (white text with a shadow, no box), brand (Page2Cam navy and blue); colour and background override the text and box colours. \
Animations (enter/exit): none, fade (default), slide-left, slide-right, slide-up, slide-down. hide_after_s hides it by itself. \
Sound of unmuted videos goes to the stream only (with page sound). Layers come back hidden after a restart. \
template: a ready-made graphic (template = an id from list_templates, params = its values; geometry defaults from the template; \
guide <template id> lists the params); update_layer with params changes values live without reloading (null resets one). \
A picture param may be given as {\"path\": \"C:\\\\...\\\\logo.png\"} (read and embedded, up to 1 MB). \
Check the result with snapshot.";

/// The `add_layer` (`add`) or `update_layer` input schema.
fn layer_schema(add: bool) -> Value {
    let unit = json!({ "type": "number", "minimum": 0, "maximum": 1 });
    let size = json!({ "type": "number", "minimum": 0.01, "maximum": 1 });
    let colour = json!({ "type": ["string", "null"], "pattern": "^#[0-9a-fA-F]{6}$" });
    let anim =
        json!({ "enum": ["none", "fade", "slide-left", "slide-right", "slide-up", "slide-down"] });
    let text = json!({ "type": "string", "maxLength": MAX_LAYER_TEXT });
    let mut props = json!({
        "id": { "type": "string", "maxLength": 40 },
        "name": { "type": "string", "maxLength": 80 },
        "x": unit, "y": unit, "w": size, "h": size,
        "z": { "type": "integer", "minimum": -1000, "maximum": 1000 },
        "visible": { "type": "boolean" }, "opacity": unit,
        "enter": anim, "exit": anim,
        "hide_after_s": { "type": ["integer", "null"], "minimum": 1, "maximum": 3600 },
        "text": text, "subtitle": text,
        "style": { "enum": ["news", "minimal", "brand"] },
        "colour": colour, "background": colour,
        "items": { "type": "array", "minItems": 1, "maxItems": 50, "items": text },
        "speed": { "type": "integer", "minimum": 10, "maximum": 1000 },
        "html": { "type": "string", "maxLength": MAX_LAYER_HTML },
        "path": { "type": "string", "maxLength": 1024 },
        "data_base64": { "type": "string" },
        "url": { "type": "string", "maxLength": MAX_URL_BYTES },
        "link": { "type": "string", "maxLength": 2048 },
        "start_s": { "type": "integer", "minimum": 0, "maximum": 86400 },
        "loop": { "type": "boolean" }, "muted": { "type": "boolean" },
        "template": { "type": "string", "maxLength": 40 },
        "params": { "type": "object" },
    });
    if add {
        props["kind"] =
            json!({ "enum": ["title", "ticker", "html", "template", "image", "video", "youtube"] });
        json!({ "type": "object", "required": ["kind"], "additionalProperties": false, "properties": props })
    } else {
        json!({ "type": "object", "required": ["id"], "additionalProperties": false, "minProperties": 2, "properties": props })
    }
}

/// Longest title, subtitle or ticker item.
const MAX_LAYER_TEXT: usize = 500;
/// Largest `html` layer.
pub const MAX_LAYER_HTML: usize = 256 * 1024;

/// Every field a layer call may carry (besides `kind`).
const LAYER_FIELDS: &[&str] = &[
    "id",
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
    "text",
    "subtitle",
    "style",
    "colour",
    "background",
    "items",
    "speed",
    "html",
    "path",
    "data_base64",
    "url",
    "link",
    "start_s",
    "loop",
    "muted",
    "template",
    "params",
];

fn int_arg(args: &Args, k: &str, lo: i64, hi: i64) -> Result<Option<i64>, String> {
    match args.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_f64() {
            Some(n) if n.fract() == 0.0 && (lo as f64..=hi as f64).contains(&n) => {
                Ok(Some(n as i64))
            }
            _ => Err(format!("{k} must be a whole number from {lo} to {hi}")),
        },
    }
}

fn text_arg(args: &Args, k: &str, max_chars: usize) -> Result<Option<String>, String> {
    match str_arg(args, k)? {
        Some(s) if s.chars().count() > max_chars => {
            Err(format!("{k} is longer than {max_chars} characters"))
        }
        s => Ok(s.map(String::from)),
    }
}

/// Check the layer fields of `args` (types and ranges; the engine checks
/// which fields fit the kind) and build the engine body.
fn layer_body(args: &Args) -> Result<Map<String, Value>, String> {
    let mut m = Map::new();
    if let Some(id) = text_arg(args, "id", 40)? {
        m.insert("id".into(), id.into());
    }
    if let Some(n) = text_arg(args, "name", 80)? {
        m.insert("name".into(), n.into());
    }
    for (k, lo) in [
        ("x", 0.0),
        ("y", 0.0),
        ("w", 0.01),
        ("h", 0.01),
        ("opacity", 0.0),
    ] {
        if let Some(v) = num_arg(args, k, lo, 1.0)? {
            m.insert(k.into(), v.into());
        }
    }
    if let Some(z) = int_arg(args, "z", -1000, 1000)? {
        m.insert("z".into(), z.into());
    }
    for k in ["visible", "loop", "muted"] {
        if let Some(b) = bool_arg(args, k)? {
            m.insert(k.into(), b.into());
        }
    }
    for k in ["enter", "exit"] {
        if let Some(a) = str_arg(args, k)? {
            let a = one_of(
                k,
                a,
                &[
                    "none",
                    "fade",
                    "slide-left",
                    "slide-right",
                    "slide-up",
                    "slide-down",
                ],
            )?;
            m.insert(k.into(), a.into());
        }
    }
    match args.get("hide_after_s") {
        Some(Value::Null) => {
            m.insert("hide_after_s".into(), Value::Null);
        }
        _ => {
            if let Some(s) = int_arg(args, "hide_after_s", 1, 3600)? {
                m.insert("hide_after_s".into(), s.into());
            }
        }
    }
    for k in ["text", "subtitle"] {
        if let Some(t) = text_arg(args, k, MAX_LAYER_TEXT)? {
            m.insert(k.into(), t.into());
        }
    }
    if let Some(s) = str_arg(args, "style")? {
        m.insert(
            "style".into(),
            one_of("style", s, &["news", "minimal", "brand"])?.into(),
        );
    }
    for k in ["colour", "background"] {
        match args.get(k) {
            Some(Value::Null) => {
                m.insert(k.into(), Value::Null);
            }
            _ => {
                if let Some(c) = colour_arg(args, k)? {
                    m.insert(k.into(), c.into());
                }
            }
        }
    }
    if let Some(v) = args.get("items").filter(|v| !v.is_null()) {
        let items = v
            .as_array()
            .filter(|a| !a.is_empty() && a.len() <= 50)
            .ok_or("items must be a list of 1 to 50 texts")?;
        for i in items {
            match i.as_str() {
                Some(s) if s.chars().count() <= MAX_LAYER_TEXT => {}
                _ => {
                    return Err(format!(
                        "each item must be text of at most {MAX_LAYER_TEXT} characters"
                    ))
                }
            }
        }
        m.insert("items".into(), v.clone());
    }
    if let Some(s) = int_arg(args, "speed", 10, 1000)? {
        m.insert("speed".into(), s.into());
    }
    if let Some(h) = str_arg(args, "html")? {
        if h.trim().is_empty() {
            return Err("html is empty".into());
        }
        if h.len() > MAX_LAYER_HTML {
            return Err(
                "html is larger than 256 KB; link big assets instead of inlining them".into(),
            );
        }
        m.insert("html".into(), h.into());
    }
    match (str_arg(args, "path")?, str_arg(args, "data_base64")?) {
        (Some(_), Some(_)) => return Err("give either path or data_base64".into()),
        (Some(p), None) => {
            if !is_local_drive_path(p) {
                return Err("path must be a local file on a drive (like C:\\Videos\\clip.mp4); network shares are not allowed".into());
            }
            m.insert("path".into(), p.into());
        }
        (None, Some(b)) => {
            if b.len() / 4 * 3 > MAX_IMAGE_BYTES + 3 {
                return Err("the picture is larger than 20 MB (give videos by path)".into());
            }
            let clean: String = b.chars().filter(|c| !c.is_ascii_whitespace()).collect();
            base64::engine::general_purpose::STANDARD
                .decode(&clean)
                .map_err(|_| "data_base64 is not valid base64".to_string())?;
            m.insert("data_base64".into(), clean.into());
        }
        (None, None) => {}
    }
    if let Some(u) = str_arg(args, "url")? {
        let l = u.trim().to_ascii_lowercase();
        if !(l.starts_with("https://") || l.starts_with("http://")) || u.len() > MAX_URL_BYTES {
            return Err("url must be an http:// or https:// address".into());
        }
        m.insert("url".into(), u.trim().into());
    }
    if let Some(l) = text_arg(args, "link", 2048)? {
        if l.trim().is_empty() {
            return Err("link must be a YouTube link or video id".into());
        }
        m.insert("link".into(), l.trim().into());
    }
    if let Some(s) = int_arg(args, "start_s", 0, 86400)? {
        m.insert("start_s".into(), s.into());
    }
    if let Some(t) = str_arg(args, "template")? {
        let t = t.trim();
        if !is_simple_id(t) {
            return Err("template must be an id from list_templates".into());
        }
        m.insert("template".into(), t.into());
    }
    match args.get("params") {
        None => {}
        Some(Value::Object(p)) => {
            let mut p = p.clone();
            embed_paths(&mut p)?;
            if serde_json::to_string(&p).map_or(0, |s| s.len()) > MAX_PARAMS_BYTES {
                return Err("params are larger than 2 MB".into());
            }
            m.insert("params".into(), Value::Object(p));
        }
        Some(_) => return Err("params must be an object of parameter values".into()),
    }
    Ok(m)
}

/// Largest parameter set of a template layer.
const MAX_PARAMS_BYTES: usize = 2 * 1024 * 1024;
/// Largest picture a template param may embed.
const MAX_PARAM_PICTURE: usize = 1024 * 1024;

/// `[A-Za-z0-9_-]`, 1..=40.
fn is_simple_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Template params given as `{"path": "C:\\…\\logo.png"}` become
/// `data:image/…;base64,` URIs (local drives only, pictures up to 1 MB).
fn embed_paths(params: &mut Map<String, Value>) -> Result<(), String> {
    for (k, v) in params.iter_mut() {
        let Some(o) = v.as_object() else { continue };
        let Some(path) = o.get("path").and_then(Value::as_str) else {
            return Err(format!(
                "params.{k}: give a value, or {{\"path\": \"C:\\\\...\\\\picture.png\"}} for a picture"
            ));
        };
        if o.len() != 1 {
            return Err(format!("params.{k}: give only path"));
        }
        if !is_local_drive_path(path) {
            return Err(format!(
                "params.{k}: path must be a local file on a drive (like C:\\Pictures\\logo.png); network shares are not allowed"
            ));
        }
        let meta = std::fs::metadata(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        if !meta.is_file() || meta.len() as usize > MAX_PARAM_PICTURE {
            return Err(format!(
                "params.{k}: {path} must be a picture file of at most 1 MB"
            ));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "png"
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            "jpeg"
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            "webp"
        } else if bytes.starts_with(b"GIF8") {
            "gif"
        } else {
            return Err(format!(
                "params.{k}: {path} is not a PNG, JPEG, WebP or GIF picture"
            ));
        };
        *v = Value::String(format!(
            "data:image/{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        ));
    }
    Ok(())
}

fn layer_id(args: &Args) -> Result<String, String> {
    let id = req_str(args, "id")?.trim();
    if id.is_empty()
        || id.len() > 40
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("id must be a layer id from list_layers".into());
    }
    Ok(id.to_string())
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
        "list_screens" => return simple(C::ListScreens),
        "open_url" => (|| {
            only(args, &["url"])?;
            let u = req_str(args, "url")?.trim();
            if u.is_empty() || u.len() > MAX_URL_BYTES {
                return Err("url must be 1 to 8192 characters".into());
            }
            if !file_url_is_local(u) {
                return Err("file: URLs must name a local file (file:///C:/...); network shares are not allowed".into());
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
                    "screen",
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
                    &[
                        "page",
                        "image",
                        "colour",
                        "youtube",
                        "screen",
                        "blurred-room",
                    ],
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
            if let Some(s) = str_arg(args, "screen")? {
                let s = s.trim();
                if s.len() > 64 {
                    return Err("screen must be an id from list_screens".into());
                }
                m.insert("full_screen".into(), s.into());
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
        "list_layers" => return simple(C::ListLayers),
        "clear_layers" => return simple(C::ClearLayers),
        "add_layer" => (|| {
            let mut allowed = LAYER_FIELDS.to_vec();
            allowed.push("kind");
            only(args, &allowed)?;
            let kind = one_of(
                "kind",
                req_str(args, "kind")?,
                &[
                    "title", "ticker", "html", "template", "image", "video", "youtube",
                ],
            )?;
            let mut m = layer_body(args)?;
            m.insert("kind".into(), kind.into());
            Ok(C::AddLayer(Value::Object(m)))
        })(),
        "update_layer" => (|| {
            only(args, LAYER_FIELDS)?;
            let id = layer_id(args)?;
            let mut m = layer_body(args)?;
            m.remove("id");
            if m.is_empty() {
                return Err("give at least one field to change besides id".into());
            }
            Ok(C::UpdateLayer {
                id,
                patch: Value::Object(m),
            })
        })(),
        "show_layer" | "hide_layer" | "remove_layer" => (|| {
            only(args, &["id"])?;
            let id = layer_id(args)?;
            Ok(match name {
                "show_layer" => C::ShowLayer(id),
                "hide_layer" => C::HideLayer(id),
                _ => C::RemoveLayer(id),
            })
        })(),
        "reorder_layers" => (|| {
            only(args, &["ids"])?;
            let ids = args
                .get("ids")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty() && a.len() <= 32)
                .ok_or("ids must be a list of 1 to 32 layer ids")?;
            let ids = ids
                .iter()
                .map(|v| {
                    v.as_str()
                        .filter(|s| !s.is_empty() && s.len() <= 40)
                        .map(String::from)
                        .ok_or_else(|| "each id must be a layer id from list_layers".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(C::ReorderLayers(ids))
        })(),
        "guide" => (|| {
            only(args, &["topic"])?;
            let t = text_arg(args, "topic", 200)?;
            Ok(C::Guide(t.filter(|s| !s.trim().is_empty())))
        })(),
        "list_templates" => (|| {
            only(args, &["category"])?;
            let c = text_arg(args, "category", 40)?.filter(|s| !s.trim().is_empty());
            if let Some(c) = &c {
                if !is_simple_id(c.trim()) {
                    return Err("category must be one of basics, time, cards, lists, data, cta, calm, software, mine".into());
                }
            }
            Ok(C::ListTemplates(c.map(|c| c.trim().to_string())))
        })(),
        "get_template" | "delete_template" | "delete_scene" => (|| {
            let allowed: &[&str] = if name == "get_template" {
                &["id", "include_html"]
            } else {
                &["id"]
            };
            only(args, allowed)?;
            let id = req_str(args, "id")?.trim().to_string();
            if !is_simple_id(&id) {
                return Err(format!(
                    "id must be an id from {}",
                    if name == "delete_scene" {
                        "list_scenes"
                    } else {
                        "list_templates"
                    }
                ));
            }
            Ok(match name {
                "get_template" => C::GetTemplate {
                    id,
                    include_html: bool_arg(args, "include_html")?.unwrap_or(false),
                },
                "delete_template" => C::DeleteTemplate(id),
                _ => C::DeleteScene(id),
            })
        })(),
        "save_template" => (|| {
            only(
                args,
                &[
                    "id",
                    "name",
                    "category",
                    "description",
                    "html",
                    "params",
                    "example",
                    "update",
                    "rect",
                    "full_frame",
                    "from_layer",
                    "copy_of",
                ],
            )?;
            if args.is_empty() {
                return Err("give a template definition, from_layer or copy_of".into());
            }
            if let Some(h) = str_arg(args, "html")? {
                if h.len() > MAX_LAYER_HTML {
                    return Err("html is larger than 256 KB".into());
                }
            }
            for k in ["id", "from_layer", "copy_of"] {
                if let Some(v) = str_arg(args, k)? {
                    if !is_simple_id(v) {
                        return Err(format!("{k} must be an id (a-z, 0-9 and -)"));
                    }
                }
            }
            if args.contains_key("from_layer") && args.contains_key("copy_of") {
                return Err("give from_layer or copy_of, not both".into());
            }
            if !args.contains_key("from_layer") && !args.contains_key("copy_of") {
                for k in ["id", "name", "html"] {
                    if !args.contains_key(k) {
                        return Err(format!(
                            "{k} is required for a new template (or use from_layer / copy_of)"
                        ));
                    }
                }
            }
            Ok(C::SaveTemplate(Value::Object(args.clone())))
        })(),
        "list_scenes" => return simple(C::ListScenes),
        "save_scene" => (|| {
            only(args, &["name", "include_background", "include_webcam"])?;
            let n = req_str(args, "name")?.trim();
            if n.is_empty() || n.chars().count() > 80 {
                return Err("name must be 1 to 80 characters".into());
            }
            Ok(C::SaveScene {
                name: n.to_string(),
                include_background: bool_arg(args, "include_background")?.unwrap_or(false),
                include_webcam: bool_arg(args, "include_webcam")?.unwrap_or(false),
            })
        })(),
        "load_scene" => (|| {
            only(args, &["id", "mode"])?;
            let id = req_str(args, "id")?.trim().to_string();
            if !is_simple_id(&id) {
                return Err("id must be an id from list_scenes".into());
            }
            let replace = match str_arg(args, "mode")?.unwrap_or("replace") {
                "replace" => true,
                "add" => false,
                _ => return Err("mode must be replace or add".into()),
            };
            Ok(C::LoadScene { id, replace })
        })(),
        _ => return Err(None),
    };
    r.map_err(Some)
}

/// Non-`file:` URLs pass; a `file:` URL only as `file:///<path>` or
/// `file://localhost/<path>` (no host: a UNC host would make Windows send
/// the user's credentials there).
fn file_url_is_local(u: &str) -> bool {
    let Some(scheme) = u.get(..5) else {
        return true;
    };
    if !scheme.eq_ignore_ascii_case("file:") {
        return true;
    }
    let rest = &u[5..];
    if rest.contains('\\') {
        return false;
    }
    let lower = rest.to_ascii_lowercase();
    let path = if let Some(p) = lower.strip_prefix("//localhost/") {
        p
    } else if let Some(p) = lower.strip_prefix("///") {
        p
    } else {
        return false;
    };
    // `file:////host/...` is a share too.
    !path.starts_with('/')
}

/// A path on a local drive (`C:\...`; elsewhere an absolute `/...`), never
/// a network share (`\\host\...`, `//host/...`, `\\?\UNC\...`).
fn is_local_drive_path(p: &str) -> bool {
    if cfg!(windows) {
        let b = p.as_bytes();
        b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'\\' || b[2] == b'/')
    } else {
        p.starts_with('/') && !p.starts_with("//")
    }
}

fn read_picture(path: &str) -> Result<Vec<u8>, String> {
    if !is_local_drive_path(path) {
        return Err("path must be a local file on a drive (like C:\\Pictures\\a.png); network shares are not allowed".into());
    }
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
