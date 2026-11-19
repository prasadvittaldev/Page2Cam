//! HTTP + WebSocket server on 127.0.0.1.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State as AxState};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use p2c_compositor::PipLayout;
use p2c_core::FrameSlot;
use serde::Deserialize;
use tokio::sync::watch;

use crate::api::{
    AudioSettings, BackgroundDto, Controls, CropDto, GpuInfo, ImageRow, InputEvent, KeyAction,
    MouseBtn, NavAction, PipDto, StreamRequest,
};
use crate::input::{map_preview, normalize_url};
use crate::preview::spawn_encoder;
use crate::tray::TrayLink;

const INDEX_HTML: &str = include_str!("../ui/index.html");
const LOGO_PNG: &[u8] = include_bytes!("../ui/logo.png");
const ICON_PNG: &[u8] = include_bytes!("../ui/icon.png");

type Jpeg = Option<Arc<Vec<u8>>>;

pub struct AdminConfig {
    /// Preferred port; falls back to any free port if taken (unless `strict_port`).
    pub port: u16,
    pub width: u32,
    pub height: u32,
    pub open_browser: bool,
    /// Access token to use (engine mode: given by the tray); `None` generates one.
    pub token: Option<String>,
    /// Fail with [`PortUnavailable`] instead of falling back to another port.
    pub strict_port: bool,
}

/// `strict_port` was set and `port` could not be bound.
#[derive(Debug)]
pub struct PortUnavailable {
    pub port: u16,
    pub source: std::io::Error,
}

impl std::fmt::Display for PortUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The cause comes from `source` (`{:#}` prints the chain).
        write!(f, "admin port 127.0.0.1:{} unavailable", self.port)
    }
}

impl std::error::Error for PortUnavailable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub struct AdminServer {
    pub addr: SocketAddr,
    /// Full URL including the access token.
    pub url: String,
    /// Base of the generated background pages, ending in `/`:
    /// `http://127.0.0.1:<port>/bg/<per-launch random id>/` (no token needed;
    /// the unguessable id and the Host check guard it).
    pub bg_url: String,
}

#[derive(Clone)]
pub(crate) struct State {
    controls: Arc<dyn Controls>,
    token: Arc<str>,
    port: u16,
    width: u32,
    height: u32,
    preview: Arc<watch::Sender<Jpeg>>,
    tray: Option<Arc<TrayLink>>,
    /// Per-launch random id of the background pages (`/bg/<id>/…`).
    pub(crate) bg_id: Arc<str>,
}

impl State {
    pub(crate) fn new(
        controls: Arc<dyn Controls>,
        token: String,
        port: u16,
        width: u32,
        height: u32,
    ) -> Self {
        let (tx, _rx) = watch::channel(None);
        State {
            controls,
            token: token.into(),
            port,
            width,
            height,
            preview: Arc::new(tx),
            tray: TrayLink::global(),
            bg_id: random_token().into(),
        }
    }
}

pub(crate) fn router(st: State) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/state", get(state))
        .route("/ui/logo.png", get(|| async { png(LOGO_PNG) }))
        .route("/ui/icon.png", get(|| async { png(ICON_PNG) }))
        .route("/api/navigate", post(navigate))
        .route("/api/nav", post(nav))
        .route("/api/pip", post(pip))
        .route("/api/cutout", post(cutout))
        .route("/api/update/install", post(update_install))
        .route("/api/gpus", get(gpus))
        .route("/api/gpu", post(gpu))
        .route("/api/restart", post(restart))
        .route("/api/quit", post(quit))
        .route("/api/stream/start", post(stream_start))
        .route("/api/stream/stop", post(stream_stop))
        .route("/api/audio", post(audio))
        .route("/api/crop", get(crop_get).post(crop_set))
        .route("/api/input", post(input))
        .route("/api/snapshot", get(snapshot))
        .route("/api/agents", get(agents).post(agents))
        .route("/api/background", get(background_get).post(background_set))
        .route(
            "/api/background/image",
            post(background_upload)
                .get(background_image_get)
                // Pictures up to 20 MB (other routes keep the 2 MB limit).
                .layer(axum::extract::DefaultBodyLimit::max(MAX_IMAGE_BYTES)),
        )
        .route("/api/background/thumb", get(background_thumb))
        .route("/bg/{id}/current.html", get(bg_page))
        .route("/bg/{id}/image", get(bg_image))
        .route("/ws", get(ws))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(st.clone(), guard))
        .with_state(st)
}

/// Start the admin server on its own runtime thread. Returns once listening.
pub fn serve(
    cfg: AdminConfig,
    controls: Arc<dyn Controls>,
    preview_frames: FrameSlot,
) -> anyhow::Result<AdminServer> {
    let token = cfg.token.clone().unwrap_or_else(random_token);
    let (tx, rx) = std::sync::mpsc::channel::<anyhow::Result<(SocketAddr, Arc<str>)>>();
    let token2 = token.clone();
    std::thread::Builder::new()
        .name("p2c-admin".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(e.into()));
                    return;
                }
            };
            rt.block_on(async move {
                let listener = match tokio::net::TcpListener::bind(("127.0.0.1", cfg.port)).await {
                    Ok(l) => l,
                    Err(e) if cfg.strict_port => {
                        let _ = tx.send(Err(PortUnavailable {
                            port: cfg.port,
                            source: e,
                        }
                        .into()));
                        return;
                    }
                    Err(_) => match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
                        Ok(l) => l,
                        Err(e) => {
                            let _ = tx.send(Err(e.into()));
                            return;
                        }
                    },
                };
                let addr = listener.local_addr().expect("bound socket has an address");
                let st = State::new(controls, token2, addr.port(), cfg.width, cfg.height);
                spawn_encoder(preview_frames, st.preview.clone());
                let _ = tx.send(Ok((addr, st.bg_id.clone())));
                if let Err(e) = axum::serve(listener, router(st)).await {
                    tracing::error!(error = %e, "admin server stopped");
                }
            });
        })?;
    let (addr, bg_id) = rx.recv()??;
    let bg_url = format!("http://127.0.0.1:{}/bg/{bg_id}/", addr.port());
    let url = format!("http://127.0.0.1:{}/?t={}", addr.port(), token);
    if cfg.open_browser {
        if let Err(e) = webbrowser::open(&url) {
            tracing::warn!(error = %e, "could not open a browser; open the admin URL manually");
        }
    }
    Ok(AdminServer { addr, url, bg_url })
}

fn random_token() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..32)
        .map(|_| format!("{:x}", rng.random_range(0..16u8)))
        .collect()
}

/// Only our own origin with the right token gets in (blocks other local web
/// pages and DNS-rebinding attacks).
async fn guard(AxState(st): AxState<State>, req: Request, next: Next) -> Response {
    let host_ok = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| {
            h == format!("127.0.0.1:{}", st.port) || h == format!("localhost:{}", st.port)
        });
    let header_token = req
        .headers()
        .get("x-p2c-token")
        .and_then(|v| v.to_str().ok());
    let query_token = req
        .uri()
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("t=")));
    // Background pages: Chromium loads them without the token. The Host
    // check stays; the per-launch id must match, and only the page and its
    // image are reachable this way.
    if let Some(rest) = req.uri().path().strip_prefix("/bg/") {
        if !host_ok {
            return (StatusCode::FORBIDDEN, "forbidden").into_response();
        }
        let known = rest.split_once('/').is_some_and(|(id, file)| {
            constant_time_eq(id.as_bytes(), st.bg_id.as_bytes())
                && matches!(file, "current.html" | "image")
        });
        if !known {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
        return next.run(req).await;
    }
    let token_ok = header_token
        .or(query_token)
        .is_some_and(|t| constant_time_eq(t.as_bytes(), st.token.as_bytes()));
    if !(host_ok && token_ok) {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn png(bytes: &'static [u8]) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "max-age=86400"),
        ],
        bytes,
    )
}

async fn index() -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-store")], Html(INDEX_HTML))
}

async fn state(AxState(st): AxState<State>) -> impl IntoResponse {
    let c = &st.controls;
    Json(serde_json::json!({
        "status": c.status(),
        "pip": PipDto::from(c.pip()),
        "audio": c.audio(),
        "crop": c.crop(),
    }))
}

#[derive(Deserialize)]
struct NavigateBody {
    url: String,
}

async fn navigate(AxState(st): AxState<State>, Json(b): Json<NavigateBody>) -> Response {
    match normalize_url(&b.url) {
        Ok(url) => {
            st.controls.navigate(&url);
            Json(serde_json::json!({ "url": url })).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct NavBody {
    action: NavAction,
}

async fn nav(AxState(st): AxState<State>, Json(b): Json<NavBody>) -> StatusCode {
    st.controls.nav(b.action);
    StatusCode::OK
}

async fn pip(AxState(st): AxState<State>, Json(b): Json<PipDto>) -> Response {
    match PipLayout::try_from(b) {
        Ok(l) => {
            st.controls.set_pip(l);
            StatusCode::OK.into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct CutoutBody {
    engine: String,
}

async fn update_install(AxState(st): AxState<State>) -> Response {
    // Downloading 300 MB takes a while: run it off the async runtime.
    let c = st.controls.clone();
    match tokio::task::spawn_blocking(move || c.install_update()).await {
        Ok(Ok(())) => StatusCode::OK.into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn cutout(AxState(st): AxState<State>, Json(b): Json<CutoutBody>) -> Response {
    match st.controls.set_cutout_engine(&b.engine) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

async fn gpus(AxState(st): AxState<State>) -> Json<Vec<GpuInfo>> {
    Json(st.controls.gpus())
}

#[derive(Deserialize)]
struct GpuBody {
    choice: String,
}

async fn gpu(AxState(st): AxState<State>, Json(b): Json<GpuBody>) -> Response {
    match st.controls.set_gpu_choice(&b.choice) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

async fn restart(AxState(st): AxState<State>) -> Response {
    match st.controls.restart() {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// Close the app the normal way (as Ctrl-C does); answers before closing.
async fn quit(AxState(st): AxState<State>) -> StatusCode {
    st.controls.quit();
    StatusCode::OK
}

async fn stream_start(AxState(st): AxState<State>, Json(b): Json<StreamRequest>) -> Response {
    match st.controls.start_stream(b) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

async fn stream_stop(AxState(st): AxState<State>) -> StatusCode {
    st.controls.stop_stream();
    StatusCode::OK
}

async fn audio(AxState(st): AxState<State>, Json(b): Json<AudioSettings>) -> StatusCode {
    st.controls.set_audio(b);
    StatusCode::OK
}

async fn crop_get(AxState(st): AxState<State>) -> Json<CropDto> {
    Json(st.controls.crop())
}

#[derive(Deserialize)]
struct CropBody {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// Write it to the settings file (the end of a drag); live updates don't.
    #[serde(default)]
    save: bool,
}

/// Answers with the crop as applied (clamped).
async fn crop_set(AxState(st): AxState<State>, Json(b): Json<CropBody>) -> Json<CropDto> {
    let c = CropDto {
        x: b.x,
        y: b.y,
        w: b.w,
        h: b.h,
    }
    .clamped();
    st.controls.set_crop(c, b.save);
    Json(c)
}

fn json_error(code: StatusCode, msg: &str) -> Response {
    (code, Json(serde_json::json!({ "error": msg }))).into_response()
}

/// `POST /api/input` from agents: x/y are 0..1 of the output frame.
#[derive(Deserialize)]
struct InputBody {
    kind: String,
    x: Option<f32>,
    y: Option<f32>,
    dx: Option<f32>,
    dy: Option<f32>,
    key: Option<String>,
    text: Option<String>,
}

const MAX_TEXT_CHARS: usize = 10_000;
/// Largest request body any route accepts.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Windows virtual-key code for a named key.
fn named_key(name: &str) -> Option<i32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => 13,
        "tab" => 9,
        "escape" | "esc" => 27,
        "backspace" => 8,
        "delete" | "del" => 46,
        "space" => 32,
        "arrowleft" | "left" => 37,
        "arrowup" | "up" => 38,
        "arrowright" | "right" => 39,
        "arrowdown" | "down" => 40,
        "home" => 36,
        "end" => 35,
        "pageup" => 33,
        "pagedown" => 34,
        _ => return None,
    })
}

fn input_events(b: InputBody, w: u32, h: u32) -> Result<Vec<InputEvent>, String> {
    let key = |action, code, ch| InputEvent::Key {
        action,
        code,
        ch,
        mods: 0,
    };
    match b.kind.as_str() {
        "click" | "move" | "scroll" => {
            let (Some(x), Some(y)) = (b.x, b.y) else {
                return Err("x and y are required".into());
            };
            if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
                return Err("x and y must be between 0 and 1".into());
            }
            let (px, py) = map_preview(x as f64, y as f64, w, h);
            Ok(match b.kind.as_str() {
                "move" => vec![InputEvent::Move {
                    x: px,
                    y: py,
                    mods: 0,
                }],
                "click" => {
                    let btn = |down| InputEvent::Button {
                        x: px,
                        y: py,
                        button: MouseBtn::Left,
                        down,
                        clicks: 1,
                        mods: 0,
                    };
                    vec![
                        InputEvent::Move {
                            x: px,
                            y: py,
                            mods: 0,
                        },
                        btn(true),
                        btn(false),
                    ]
                }
                _ => {
                    let px_of = |v: Option<f32>| {
                        v.filter(|v| v.is_finite())
                            .unwrap_or(0.0)
                            .clamp(-100_000.0, 100_000.0) as i32
                    };
                    vec![InputEvent::Wheel {
                        x: px,
                        y: py,
                        dx: px_of(b.dx),
                        dy: px_of(b.dy),
                        mods: 0,
                    }]
                }
            })
        }
        "key" => {
            let name = b.key.ok_or("key is required")?;
            let code = named_key(&name).ok_or_else(|| format!("unknown key {name:?}"))?;
            let mut v = vec![key(KeyAction::Down, code, 0)];
            if code == 13 || code == 32 {
                v.push(key(KeyAction::Char, code, code as u16));
            }
            v.push(key(KeyAction::Up, code, 0));
            Ok(v)
        }
        "text" => {
            let text = b.text.ok_or("text is required")?;
            if text.chars().count() > MAX_TEXT_CHARS {
                return Err(format!("text is longer than {MAX_TEXT_CHARS} characters"));
            }
            // Newlines are Enter (CR); CRLF counts once.
            Ok(text
                .replace("\r\n", "\n")
                .replace('\n', "\r")
                .encode_utf16()
                .map(|u| key(KeyAction::Char, 0, u))
                .collect())
        }
        other => Err(format!("unknown input kind {other:?}")),
    }
}

/// The page is rendered at the output size (`cfg.width` x `cfg.height`), never
/// letterboxed, so output 0..1 maps straight to page px (same as the WebSocket).
async fn input(AxState(st): AxState<State>, Json(b): Json<InputBody>) -> Response {
    match input_events(b, st.width, st.height) {
        Ok(events) => {
            for ev in events {
                st.controls.input(ev);
            }
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Err(e) => json_error(StatusCode::BAD_REQUEST, &e),
    }
}

async fn snapshot(AxState(st): AxState<State>) -> Response {
    let controls = st.controls.clone();
    let jpg = tokio::task::spawn_blocking(move || controls.snapshot())
        .await
        .ok()
        .flatten();
    match jpg {
        Some(b) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            b,
        )
            .into_response(),
        None => json_error(StatusCode::SERVICE_UNAVAILABLE, "no frame yet"),
    }
}

/// `/api/agents`: forwarded to the tray's `/tray/agents`.
async fn agents(
    AxState(st): AxState<State>,
    method: axum::http::Method,
    body: axum::body::Bytes,
) -> Response {
    let Some(link) = st.tray.clone() else {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "tray not available");
    };
    let is_post = method == axum::http::Method::POST;
    let res = tokio::task::spawn_blocking(move || {
        link.request(
            if is_post { "POST" } else { "GET" },
            is_post.then_some(&body[..]),
        )
    })
    .await;
    match res {
        Ok(Ok((code, bytes))) => (
            StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
            [(header::CONTENT_TYPE, "application/json")],
            bytes,
        )
            .into_response(),
        _ => json_error(StatusCode::BAD_GATEWAY, "tray did not answer"),
    }
}
/// Largest background picture upload.
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
/// Longest a thumbnail request waits for the engine's fetch.
const THUMB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// PNG, JPEG or WebP, by the first bytes (never by the file name).
fn looks_like_image(b: &[u8]) -> bool {
    b.starts_with(b"\x89PNG\r\n\x1a\n")
        || b.starts_with(&[0xff, 0xd8, 0xff])
        || (b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP")
}

async fn background_get(AxState(st): AxState<State>) -> Json<BackgroundDto> {
    Json(st.controls.background())
}

async fn background_set(AxState(st): AxState<State>, Json(b): Json<BackgroundDto>) -> Response {
    // May navigate and write the settings file: off the async runtime.
    let c = st.controls.clone();
    match tokio::task::spawn_blocking(move || c.set_background(b).map(|()| c.background())).await {
        Ok(Ok(dto)) => Json(dto).into_response(),
        Ok(Err(e)) => json_error(StatusCode::BAD_REQUEST, &format!("{e:#}")),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(Deserialize)]
struct RowQuery {
    row: Option<ImageRow>,
}

/// `POST /api/background/image?row=full|box`, body = the image file.
async fn background_upload(
    AxState(st): AxState<State>,
    q: Result<axum::extract::Query<RowQuery>, axum::extract::rejection::QueryRejection>,
    body: axum::body::Bytes,
) -> Response {
    let Some(row) = q.ok().and_then(|q| q.0.row) else {
        return json_error(StatusCode::BAD_REQUEST, "row must be full or box");
    };
    if !looks_like_image(&body) {
        return json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "only PNG, JPEG and WebP images are accepted",
        );
    }
    let c = st.controls.clone();
    let res = tokio::task::spawn_blocking(move || {
        c.set_background_image(row, body.to_vec())
            .map(|()| c.background())
    })
    .await;
    match res {
        Ok(Ok(dto)) => Json(dto).into_response(),
        Ok(Err(e)) => json_error(StatusCode::BAD_REQUEST, &format!("{e:#}")),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

fn image_response(img: Option<(Arc<Vec<u8>>, &'static str)>) -> Response {
    match img {
        Some((bytes, kind)) => (
            [
                (header::CONTENT_TYPE, kind),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes.as_ref().clone(),
        )
            .into_response(),
        None => json_error(StatusCode::NOT_FOUND, "no image"),
    }
}

/// `GET /api/background/image?row=full|box`: the stored picture (previews).
async fn background_image_get(
    AxState(st): AxState<State>,
    q: Result<axum::extract::Query<RowQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    match q.ok().and_then(|q| q.0.row) {
        Some(row) => image_response(st.controls.background_image(row)),
        None => json_error(StatusCode::BAD_REQUEST, "row must be full or box"),
    }
}

#[derive(Deserialize)]
struct ThumbQuery {
    link: Option<String>,
}

/// `GET /api/background/thumb[?link=…]`: the YouTube thumbnail, fetched by
/// the engine (the admin page loads nothing from outside).
async fn background_thumb(
    AxState(st): AxState<State>,
    axum::extract::Query(q): axum::extract::Query<ThumbQuery>,
) -> Response {
    let c = st.controls.clone();
    let link = q.link.filter(|l| !l.trim().is_empty());
    let job = tokio::task::spawn_blocking(move || c.youtube_thumb(link));
    match tokio::time::timeout(THUMB_TIMEOUT, job).await {
        Ok(Ok(Ok(jpg))) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "max-age=3600"),
            ],
            jpg.as_ref().clone(),
        )
            .into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::NOT_FOUND, &format!("{e:#}")),
        Ok(Err(e)) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "thumbnail fetch timed out"),
    }
}

/// What a background page may load: inline styles, its own image and the
/// YouTube embed; nothing else (no scripts of ours, no other sites).
const BG_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src 'self'; \
                      frame-src https://www.youtube-nocookie.com";

/// `GET /bg/<id>/current.html` (id checked by the guard; no token).
async fn bg_page(AxState(st): AxState<State>) -> Response {
    match st.controls.background_page() {
        Some(html) => (
            [
                (header::CACHE_CONTROL, "no-store"),
                (header::CONTENT_SECURITY_POLICY, BG_CSP),
                (header::REFERRER_POLICY, "strict-origin-when-cross-origin"),
            ],
            Html(html),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "no background page").into_response(),
    }
}

/// `GET /bg/<id>/image`: the full-frame picture (id checked by the guard).
async fn bg_image(AxState(st): AxState<State>) -> Response {
    image_response(st.controls.background_image(ImageRow::Full))
}

async fn ws(AxState(st): AxState<State>, up: WebSocketUpgrade) -> Response {
    up.on_upgrade(move |socket| handle_ws(st, socket))
}

/// Input from the preview: x/y are fractions (0..1) of the preview image.
#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum ClientMsg {
    Move {
        x: f64,
        y: f64,
        #[serde(default)]
        mods: u32,
    },
    Down {
        x: f64,
        y: f64,
        b: u8,
        #[serde(default = "one")]
        clicks: i32,
        #[serde(default)]
        mods: u32,
    },
    Up {
        x: f64,
        y: f64,
        b: u8,
        #[serde(default = "one")]
        clicks: i32,
        #[serde(default)]
        mods: u32,
    },
    Wheel {
        x: f64,
        y: f64,
        dx: i32,
        dy: i32,
        #[serde(default)]
        mods: u32,
    },
    Key {
        a: String,
        code: i32,
        #[serde(default)]
        ch: u16,
        #[serde(default)]
        mods: u32,
    },
}

fn one() -> i32 {
    1
}

fn to_input(m: ClientMsg, w: u32, h: u32) -> Option<InputEvent> {
    let btn = |b: u8| match b {
        1 => MouseBtn::Middle,
        2 => MouseBtn::Right,
        _ => MouseBtn::Left,
    };
    Some(match m {
        ClientMsg::Move { x, y, mods } => {
            let (x, y) = map_preview(x, y, w, h);
            InputEvent::Move { x, y, mods }
        }
        ClientMsg::Down {
            x,
            y,
            b,
            clicks,
            mods,
        } => {
            let (x, y) = map_preview(x, y, w, h);
            InputEvent::Button {
                x,
                y,
                button: btn(b),
                down: true,
                clicks,
                mods,
            }
        }
        ClientMsg::Up {
            x,
            y,
            b,
            clicks,
            mods,
        } => {
            let (x, y) = map_preview(x, y, w, h);
            InputEvent::Button {
                x,
                y,
                button: btn(b),
                down: false,
                clicks,
                mods,
            }
        }
        ClientMsg::Wheel { x, y, dx, dy, mods } => {
            let (x, y) = map_preview(x, y, w, h);
            InputEvent::Wheel { x, y, dx, dy, mods }
        }
        ClientMsg::Key { a, code, ch, mods } => {
            let action = match a.as_str() {
                "down" => KeyAction::Down,
                "up" => KeyAction::Up,
                "char" => KeyAction::Char,
                _ => return None,
            };
            InputEvent::Key {
                action,
                code,
                ch,
                mods,
            }
        }
    })
}

async fn handle_ws(st: State, socket: WebSocket) {
    let (mut tx, mut rx) = socket.split();
    let mut frames = st.preview.subscribe();
    let send = tokio::spawn(async move {
        while frames.changed().await.is_ok() {
            let jpg = frames.borrow_and_update().clone();
            if let Some(jpg) = jpg {
                if tx
                    .send(Message::Binary(jpg.as_ref().clone().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    });
    while let Some(Ok(msg)) = rx.next().await {
        if let Message::Text(t) = msg {
            match serde_json::from_str::<ClientMsg>(&t) {
                Ok(m) => {
                    if let Some(ev) = to_input(m, st.width, st.height) {
                        st.controls.input(ev);
                    }
                }
                Err(e) => tracing::debug!(error = %e, "bad input message"),
            }
        }
    }
    send.abort();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;

    use parking_lot_free::Fake;
    use tower::ServiceExt;

    mod parking_lot_free {
        use super::*;
        use std::sync::Mutex;

        #[derive(Default)]
        pub struct Fake {
            pub navigated: Mutex<Vec<String>>,
            pub pip: Mutex<PipLayout>,
            pub streams: Mutex<Vec<StreamRequest>>,
            pub engine: Mutex<String>,
            pub updates: Mutex<u32>,
            pub gpu: Mutex<String>,
            pub restarts: Mutex<u32>,
            pub quits: Mutex<u32>,
            pub crop: Mutex<CropDto>,
            /// The `save` flag of every `set_crop` call.
            pub crop_saves: Mutex<Vec<bool>>,
            pub events: Mutex<Vec<InputEvent>>,
            pub snap: Mutex<Option<Vec<u8>>>,
            pub background: Mutex<Option<BackgroundDto>>,
            /// (row, byte count) of every image upload that reached the app.
            pub uploads: Mutex<Vec<(ImageRow, usize)>>,
            pub page: Mutex<Option<String>>,
            pub full_image: Mutex<Option<Vec<u8>>>,
            /// The `link` of every thumbnail request.
            pub thumbs: Mutex<Vec<Option<String>>>,
        }

        fn gpu(name: &str, kind: &str) -> GpuInfo {
            GpuInfo {
                name: name.into(),
                kind: kind.into(),
            }
        }

        impl Controls for Fake {
            fn navigate(&self, url: &str) {
                self.navigated.lock().unwrap().push(url.into());
            }
            fn nav(&self, _a: NavAction) {}
            fn input(&self, ev: InputEvent) {
                self.events.lock().unwrap().push(ev);
            }
            fn snapshot(&self) -> Option<Vec<u8>> {
                self.snap.lock().unwrap().clone()
            }
            fn pip(&self) -> PipLayout {
                *self.pip.lock().unwrap()
            }
            fn set_pip(&self, l: PipLayout) {
                *self.pip.lock().unwrap() = l;
            }
            fn start_stream(&self, req: StreamRequest) -> anyhow::Result<()> {
                self.streams.lock().unwrap().push(req);
                Ok(())
            }
            fn stop_stream(&self) {}
            fn audio(&self) -> AudioSettings {
                AudioSettings::default()
            }
            fn set_audio(&self, _a: AudioSettings) {}
            fn status(&self) -> Status {
                Status {
                    url: "https://example.com".into(),
                    cutout: "Fast · CPU · 30 masks/s".into(),
                    update: crate::api::UpdateInfo {
                        state: "available".into(),
                        version: "9.9.9".into(),
                        page_url: "https://example.com/r".into(),
                        can_install: true,
                    },
                    ..Status::default()
                }
            }
            fn cutout_engine(&self) -> String {
                self.engine.lock().unwrap().clone()
            }
            fn install_update(&self) -> anyhow::Result<()> {
                *self.updates.lock().unwrap() += 1;
                Ok(())
            }
            fn set_cutout_engine(&self, engine: &str) -> anyhow::Result<()> {
                anyhow::ensure!(["quality", "fast"].contains(&engine), "unknown engine");
                *self.engine.lock().unwrap() = engine.into();
                Ok(())
            }
            fn gpus(&self) -> Vec<GpuInfo> {
                vec![
                    gpu("auto", "auto"),
                    gpu("NVIDIA RTX 2000 Ada Generation Laptop GPU", "discrete"),
                    gpu("Intel(R) Graphics", "onboard"),
                    gpu("cpu", "cpu"),
                ]
            }
            fn set_gpu_choice(&self, choice: &str) -> anyhow::Result<()> {
                anyhow::ensure!(
                    self.gpus().iter().any(|g| g.name == choice),
                    "unknown GPU {choice:?}"
                );
                *self.gpu.lock().unwrap() = choice.into();
                Ok(())
            }
            fn restart(&self) -> anyhow::Result<()> {
                *self.restarts.lock().unwrap() += 1;
                Ok(())
            }
            fn quit(&self) {
                *self.quits.lock().unwrap() += 1;
            }
            fn crop(&self) -> CropDto {
                *self.crop.lock().unwrap()
            }
            fn set_crop(&self, c: CropDto, save: bool) {
                *self.crop.lock().unwrap() = c;
                self.crop_saves.lock().unwrap().push(save);
            }
            fn background(&self) -> BackgroundDto {
                self.background.lock().unwrap().clone().unwrap_or_default()
            }
            fn set_background(&self, b: BackgroundDto) -> anyhow::Result<()> {
                if b.full == "youtube" {
                    let link = b.full_youtube.clone().unwrap_or_default();
                    anyhow::ensure!(link.contains("watch?v="), "not a YouTube video link");
                }
                *self.background.lock().unwrap() = Some(b);
                Ok(())
            }
            fn set_background_image(&self, row: ImageRow, bytes: Vec<u8>) -> anyhow::Result<()> {
                self.uploads.lock().unwrap().push((row, bytes.len()));
                Ok(())
            }
            fn background_page(&self) -> Option<String> {
                self.page.lock().unwrap().clone()
            }
            fn background_image(&self, row: ImageRow) -> Option<(Arc<Vec<u8>>, &'static str)> {
                (row == ImageRow::Full)
                    .then(|| self.full_image.lock().unwrap().clone())
                    .flatten()
                    .map(|b| (Arc::new(b), "image/jpeg"))
            }
            fn youtube_thumb(&self, link: Option<String>) -> anyhow::Result<Arc<Vec<u8>>> {
                self.thumbs.lock().unwrap().push(link.clone());
                anyhow::ensure!(link.as_deref() != Some("slow"), "never");
                Ok(Arc::new(vec![0xff, 0xd8, 0xff, 0xd9]))
            }
        }
    }

    const PORT: u16 = 8765;
    const TOKEN: &str = "abc123";

    fn app(fake: Arc<Fake>) -> axum::Router {
        router(State::new(fake, TOKEN.into(), PORT, 1920, 1080))
    }

    fn req(
        method: &str,
        uri: &str,
        host: &str,
        token: Option<&str>,
        body: Option<&str>,
    ) -> Request<Body> {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", host);
        if let Some(t) = token {
            b = b.header("x-p2c-token", t);
        }
        if body.is_some() {
            b = b.header("content-type", "application/json");
        }
        b.body(body.map(|s| Body::from(s.to_owned())).unwrap_or_default())
            .unwrap()
    }

    async fn body_string(r: axum::response::Response) -> String {
        String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
    }

    #[tokio::test]
    async fn rejects_missing_token() {
        let r = app(Arc::default())
            .oneshot(req("GET", "/api/state", "127.0.0.1:8765", None, None))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = app(Arc::default())
            .oneshot(req(
                "GET",
                "/api/state",
                "127.0.0.1:8765",
                Some("wrong"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_foreign_host() {
        let r = app(Arc::default())
            .oneshot(req(
                "GET",
                "/api/state",
                "evil.test:8765",
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn accepts_token_header() {
        let r = app(Arc::default())
            .oneshot(req(
                "GET",
                "/api/state",
                "localhost:8765",
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert!(body_string(r).await.contains("https://example.com"));
    }

    #[tokio::test]
    async fn index_requires_token_query() {
        let r = app(Arc::default())
            .oneshot(req("GET", "/", "127.0.0.1:8765", None, None))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = app(Arc::default())
            .oneshot(req("GET", "/?t=abc123", "127.0.0.1:8765", None, None))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert!(body_string(r).await.contains("<title>Page2Cam"));
    }

    #[tokio::test]
    async fn ui_has_background_section() {
        let r = app(Arc::default())
            .oneshot(req("GET", "/?t=abc123", "127.0.0.1:8765", None, None))
            .await
            .unwrap();
        let html = body_string(r).await;
        for id in [
            "h-bg",
            "bgFullRow",
            "bgBoxRow",
            "bgFullImageFile",
            "bgBoxImageFile",
            "bgFullColour",
            "bgBoxColour",
            "bgYoutube",
            "bgYoutubeErr",
            "bgYoutubeThumb",
            "bgFullImagePrev",
            "bgBoxImagePrev",
            "bgNote",
            "bgErr",
            "refinedEdges",
            "urlPaused",
        ] {
            assert!(html.contains(&format!("id=\"{id}\"")), "missing id {id}");
        }
        for v in ["page", "image", "colour", "youtube", "blurred-room"] {
            assert!(html.contains(&format!("data-bg-full=\"{v}\"")), "full {v}");
        }
        for v in ["room", "blur", "image", "colour"] {
            assert!(html.contains(&format!("data-bg-box=\"{v}\"")), "box {v}");
        }
        // The routes it talks to, and nothing fetched from outside.
        for route in [
            "/api/background\"",
            "/api/background/image?row=",
            "/api/background/thumb",
        ] {
            assert!(html.contains(route), "route {route}");
        }
        assert!(html.contains("(paused — background showing)"));
        // Broken previews (picture gone, thumbnail failed) fall back to the
        // placeholder: the thumbnail and both pictures have error handlers.
        assert!(html.contains("$(\"bgYoutubeThumb\").onerror"));
        assert!(html.contains("[\"bgFullImagePrev\", \"bgBoxImagePrev\"].forEach"));
        assert!(!html.contains("img.youtube.com"));
        assert!(!html.contains("src=\"http"));
        assert!(!html.contains("href=\"http"));
    }

    #[tokio::test]
    async fn navigate_normalizes_and_rejects() {
        let fake = Arc::new(Fake::default());
        let ok = req(
            "POST",
            "/api/navigate",
            "127.0.0.1:8765",
            Some(TOKEN),
            Some(r#"{"url":"example.com"}"#),
        );
        assert_eq!(
            app(fake.clone()).oneshot(ok).await.unwrap().status(),
            StatusCode::OK
        );
        let bad = req(
            "POST",
            "/api/navigate",
            "127.0.0.1:8765",
            Some(TOKEN),
            Some(r#"{"url":"javascript:alert(1)"}"#),
        );
        assert_eq!(
            app(fake.clone()).oneshot(bad).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            *fake.navigated.lock().unwrap(),
            vec!["https://example.com".to_string()]
        );
    }

    #[tokio::test]
    async fn pip_round_trip() {
        let fake = Arc::new(Fake::default());
        let body = r#"{"visible":true,"x":0.25,"y":0.75,"width":0.3}"#;
        let r = app(fake.clone())
            .oneshot(req(
                "POST",
                "/api/pip",
                "127.0.0.1:8765",
                Some(TOKEN),
                Some(body),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            *fake.pip.lock().unwrap(),
            PipLayout {
                visible: true,
                x: 0.25,
                y: 0.75,
                width: 0.3,
                style: p2c_compositor::OverlayStyle::Box,
            }
        );
    }

    async fn post(fake: Arc<Fake>, uri: &str, body: &str) -> StatusCode {
        app(fake)
            .oneshot(req("POST", uri, "127.0.0.1:8765", Some(TOKEN), Some(body)))
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn pip_style_round_trip() {
        let fake = Arc::new(Fake::default());
        let body = r#"{"visible":true,"x":0.5,"y":1.0,"width":0.3,"style":"cutout-full"}"#;
        assert_eq!(post(fake.clone(), "/api/pip", body).await, StatusCode::OK);
        assert_eq!(
            fake.pip.lock().unwrap().style,
            p2c_compositor::OverlayStyle::CutoutFull
        );
        assert_eq!(PipDto::from(*fake.pip.lock().unwrap()).style, "cutout-full");
    }

    #[tokio::test]
    async fn pip_style_rejects_unknown() {
        let fake = Arc::new(Fake::default());
        let body = r#"{"visible":true,"x":0.5,"y":1.0,"width":0.3,"style":"sparkles"}"#;
        assert_eq!(
            post(fake.clone(), "/api/pip", body).await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn cutout_engine_route() {
        let fake = Arc::new(Fake::default());
        assert_eq!(
            post(fake.clone(), "/api/cutout", r#"{"engine":"fast"}"#).await,
            StatusCode::OK
        );
        assert_eq!(*fake.engine.lock().unwrap(), "fast");
        assert_eq!(
            post(fake.clone(), "/api/cutout", r#"{"engine":"turbo"}"#).await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn serves_logo_images_with_token_only() {
        for path in ["/ui/logo.png", "/ui/icon.png"] {
            let ok = app(Arc::new(Fake::default()))
                .oneshot(req(
                    "GET",
                    &format!("{path}?t={TOKEN}"),
                    "127.0.0.1:8765",
                    None,
                    None,
                ))
                .await
                .unwrap();
            assert_eq!(ok.status(), StatusCode::OK, "{path}");
            assert_eq!(ok.headers()["content-type"], "image/png");
            let denied = app(Arc::new(Fake::default()))
                .oneshot(req("GET", path, "127.0.0.1:8765", None, None))
                .await
                .unwrap();
            assert_eq!(denied.status(), StatusCode::FORBIDDEN, "{path}");
        }
    }

    #[tokio::test]
    async fn update_install_route() {
        let fake = Arc::new(Fake::default());
        assert_eq!(
            post(fake.clone(), "/api/update/install", "{}").await,
            StatusCode::OK
        );
        assert_eq!(*fake.updates.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn state_includes_update() {
        let r = app(Arc::new(Fake::default()))
            .oneshot(req(
                "GET",
                "/api/state",
                "127.0.0.1:8765",
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        assert_eq!(v["status"]["update"]["version"], "9.9.9");
        assert_eq!(v["status"]["update"]["can_install"], true);
    }

    #[tokio::test]
    async fn state_includes_cutout() {
        let fake = Arc::new(Fake::default());
        let r = app(fake)
            .oneshot(req(
                "GET",
                "/api/state",
                "127.0.0.1:8765",
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        assert_eq!(v["status"]["cutout"], "Fast · CPU · 30 masks/s");
        assert_eq!(v["pip"]["style"], "box");
    }

    #[tokio::test]
    async fn gpus_route_lists_auto_first_cpu_last() {
        let r = app(Arc::new(Fake::default()))
            .oneshot(req("GET", "/api/gpus", "127.0.0.1:8765", Some(TOKEN), None))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let v: Vec<GpuInfo> = serde_json::from_str(&body_string(r).await).unwrap();
        assert_eq!(v.len(), 4);
        assert_eq!((v[0].name.as_str(), v[0].kind.as_str()), ("auto", "auto"));
        assert_eq!(v[1].kind, "discrete");
        assert_eq!((v[3].name.as_str(), v[3].kind.as_str()), ("cpu", "cpu"));
    }

    #[tokio::test]
    async fn set_gpu_saves_choice() {
        let fake = Arc::new(Fake::default());
        assert_eq!(
            post(
                fake.clone(),
                "/api/gpu",
                r#"{"choice":"Intel(R) Graphics"}"#
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(*fake.gpu.lock().unwrap(), "Intel(R) Graphics");
        assert_eq!(
            post(fake.clone(), "/api/gpu", r#"{"choice":"cpu"}"#).await,
            StatusCode::OK
        );
        assert_eq!(*fake.gpu.lock().unwrap(), "cpu");
    }

    #[tokio::test]
    async fn set_gpu_rejects_unknown_name() {
        let fake = Arc::new(Fake::default());
        assert_eq!(
            post(fake.clone(), "/api/gpu", r#"{"choice":"Voodoo 5"}"#).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(*fake.gpu.lock().unwrap(), "");
    }

    #[tokio::test]
    async fn restart_route_calls_controls() {
        let fake = Arc::new(Fake::default());
        assert_eq!(
            post(fake.clone(), "/api/restart", "{}").await,
            StatusCode::OK
        );
        assert_eq!(*fake.restarts.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn quit_route_calls_controls() {
        let fake = Arc::new(Fake::default());
        assert_eq!(post(fake.clone(), "/api/quit", "{}").await, StatusCode::OK);
        assert_eq!(*fake.quits.lock().unwrap(), 1);
        // Guarded like every route: no token or a foreign host -> 403, no quit.
        for (host, token) in [("127.0.0.1:8765", None), ("evil.test:8765", Some(TOKEN))] {
            let r = app(fake.clone())
                .oneshot(req("POST", "/api/quit", host, token, Some("{}")))
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::FORBIDDEN, "{host}");
        }
        assert_eq!(*fake.quits.lock().unwrap(), 1);
    }

    /// A plain HTTP/1.1 GET over a real socket; returns the status line.
    fn http_get(port: u16, path: &str, token: &str) -> String {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            s,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-P2C-Token: {token}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out.lines().next().unwrap_or_default().to_string()
    }

    fn admin_cfg(port: u16, token: Option<&str>, strict_port: bool) -> AdminConfig {
        AdminConfig {
            port,
            width: 1920,
            height: 1080,
            open_browser: false,
            token: token.map(str::to_string),
            strict_port,
        }
    }

    #[test]
    fn injected_token_and_strict_port() {
        // Injected token: used as given (URL and guard).
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        let tok = "0123456789abcdef0123456789abcdef";
        let srv = serve(
            admin_cfg(port, Some(tok), true),
            Arc::new(Fake::default()),
            FrameSlot::new(),
        )
        .unwrap();
        assert_eq!(srv.addr.port(), port);
        assert_eq!(srv.url, format!("http://127.0.0.1:{port}/?t={tok}"));
        assert!(http_get(port, "/api/state", tok).contains("200"));
        assert!(http_get(port, "/api/state", "nope").contains("403"));

        // Strict port in use: an error, not a fallback to another port.
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let busy_port = busy.local_addr().unwrap().port();
        let e = serve(
            admin_cfg(busy_port, Some(tok), true),
            Arc::new(Fake::default()),
            FrameSlot::new(),
        )
        .err()
        .expect("strict port in use must fail");
        let pu = e
            .downcast_ref::<PortUnavailable>()
            .expect("a PortUnavailable error");
        assert_eq!(pu.port, busy_port);
        // Not strict: falls back to a free port with a generated token.
        let srv = serve(
            admin_cfg(busy_port, None, false),
            Arc::new(Fake::default()),
            FrameSlot::new(),
        )
        .unwrap();
        assert_ne!(srv.addr.port(), busy_port);
        assert!(!srv.url.contains(tok));
        drop(busy);
    }

    /// Same guard as every route (it answers 403, not 401).
    #[tokio::test]
    async fn gpu_routes_need_token() {
        let fake = Arc::new(Fake::default());
        for (method, uri, body) in [
            ("GET", "/api/gpus", None),
            ("POST", "/api/gpu", Some(r#"{"choice":"cpu"}"#)),
            ("POST", "/api/restart", Some("{}")),
        ] {
            for host in ["127.0.0.1:8765", "evil.test:8765"] {
                let token = (host == "evil.test:8765").then_some(TOKEN);
                let r = app(fake.clone())
                    .oneshot(req(method, uri, host, token, body))
                    .await
                    .unwrap();
                assert_eq!(r.status(), StatusCode::FORBIDDEN, "{method} {uri} {host}");
            }
        }
        assert_eq!(*fake.gpu.lock().unwrap(), "");
        assert_eq!(*fake.restarts.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn state_includes_video_status() {
        let r = app(Arc::new(Fake::default()))
            .oneshot(req(
                "GET",
                "/api/state",
                "127.0.0.1:8765",
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        for k in [
            "video",
            "encoder",
            "gpu_choice",
            "gpu_running",
            "restart_needed",
            "gpu_override",
            "cutout_gpu",
        ] {
            assert!(v["status"].get(k).is_some(), "missing {k}");
        }
    }

    #[test]
    fn health_missing_fields_default() {
        let h: Health = serde_json::from_str(r#"{"output_ready":true}"#).unwrap();
        assert!(h.output_ready);
        assert_eq!(h.webcam_state, "");
        assert_eq!(
            serde_json::from_str::<Health>("{}").unwrap(),
            Health::default()
        );
    }

    #[tokio::test]
    async fn state_includes_health() {
        let r = app(Arc::new(Fake::default()))
            .oneshot(req(
                "GET",
                "/api/state",
                "127.0.0.1:8765",
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        let h = &v["status"]["health"];
        for k in [
            "output_ready",
            "output_skipped_pct",
            "webcam_fps",
            "webcam_state",
            "cutout_auto_fallback",
            "video_fallback",
            "camera_status",
        ] {
            assert!(h.get(k).is_some(), "missing health.{k}");
        }
    }

    async fn crop_json(fake: Arc<Fake>, method: &str, body: Option<&str>) -> (StatusCode, String) {
        let r = app(fake)
            .oneshot(req(
                method,
                "/api/crop",
                "127.0.0.1:8765",
                Some(TOKEN),
                body,
            ))
            .await
            .unwrap();
        let st = r.status();
        (st, body_string(r).await)
    }

    #[tokio::test]
    async fn crop_route_roundtrip() {
        let fake = Arc::new(Fake::default());
        let (st, body) = crop_json(fake.clone(), "GET", None).await;
        assert_eq!(st, StatusCode::OK);
        let c: CropDto = serde_json::from_str(&body).unwrap();
        assert_eq!((c.x, c.y, c.w, c.h), (0.0, 0.0, 1.0, 1.0));

        let (st, body) = crop_json(
            fake.clone(),
            "POST",
            Some(r#"{"x":0.25,"y":0.5,"w":0.5,"h":0.5,"save":false}"#),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let want = CropDto {
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.5,
        };
        assert_eq!(serde_json::from_str::<CropDto>(&body).unwrap(), want);
        assert_eq!(*fake.crop.lock().unwrap(), want);
        let (_, body) = crop_json(fake.clone(), "GET", None).await;
        assert_eq!(serde_json::from_str::<CropDto>(&body).unwrap(), want);
        // The UI's 1 s state poll carries it too.
        let r = app(fake)
            .oneshot(req(
                "GET",
                "/api/state",
                "127.0.0.1:8765",
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        assert_eq!(v["crop"]["w"], 0.5);
    }

    #[tokio::test]
    async fn crop_route_clamps() {
        let fake = Arc::new(Fake::default());
        let (st, body) = crop_json(
            fake.clone(),
            "POST",
            Some(r#"{"x":0.0,"y":0.0,"w":0.01,"h":0.5,"save":false}"#),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let c: CropDto = serde_json::from_str(&body).unwrap();
        assert!((c.w - 0.10).abs() < 1e-6, "{c:?}");
        assert!((fake.crop.lock().unwrap().w - 0.10).abs() < 1e-6);
        // Off the frame: pulled back inside.
        let (_, body) = crop_json(
            fake.clone(),
            "POST",
            Some(r#"{"x":0.95,"y":0.0,"w":0.5,"h":1.0,"save":false}"#),
        )
        .await;
        let c: CropDto = serde_json::from_str(&body).unwrap();
        assert!(
            (c.x - 0.5).abs() < 1e-6 && (c.w - 0.5).abs() < 1e-6,
            "{c:?}"
        );
    }

    /// Same guard as every route (it answers 403, not 401).
    #[tokio::test]
    async fn crop_route_needs_token() {
        let fake = Arc::new(Fake::default());
        for (method, body) in [
            ("GET", None),
            (
                "POST",
                Some(r#"{"x":0.25,"y":0.25,"w":0.5,"h":0.5,"save":true}"#),
            ),
        ] {
            for host in ["127.0.0.1:8765", "evil.test:8765"] {
                let token = (host == "evil.test:8765").then_some(TOKEN);
                let r = app(fake.clone())
                    .oneshot(req(method, "/api/crop", host, token, body))
                    .await
                    .unwrap();
                assert_eq!(r.status(), StatusCode::FORBIDDEN, "{method} {host}");
            }
        }
        assert_eq!(*fake.crop.lock().unwrap(), CropDto::default());
        assert!(fake.crop_saves.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn crop_save_flag_reaches_controls() {
        let fake = Arc::new(Fake::default());
        for body in [
            r#"{"x":0.1,"y":0.1,"w":0.5,"h":0.5,"save":false}"#,
            r#"{"x":0.1,"y":0.1,"w":0.5,"h":0.5,"save":true}"#,
            // `save` left out: a live update, not saved.
            r#"{"x":0.1,"y":0.1,"w":0.5,"h":0.5}"#,
        ] {
            assert_eq!(post(fake.clone(), "/api/crop", body).await, StatusCode::OK);
        }
        assert_eq!(*fake.crop_saves.lock().unwrap(), vec![false, true, false]);
    }

    #[test]
    fn ws_messages_map_to_page_pixels() {
        let m: ClientMsg = serde_json::from_str(r#"{"t":"down","x":0.5,"y":0.25,"b":0}"#).unwrap();
        assert_eq!(
            to_input(m, 1920, 1080),
            Some(InputEvent::Button {
                x: 960,
                y: 270,
                button: MouseBtn::Left,
                down: true,
                clicks: 1,
                mods: 0
            })
        );
        let k: ClientMsg =
            serde_json::from_str(r#"{"t":"key","a":"char","code":75,"ch":107}"#).unwrap();
        assert_eq!(
            to_input(k, 1920, 1080),
            Some(InputEvent::Key {
                action: KeyAction::Char,
                code: 75,
                ch: 107,
                mods: 0
            })
        );
        let bad: ClientMsg = serde_json::from_str(r#"{"t":"key","a":"zap","code":1}"#).unwrap();
        assert_eq!(to_input(bad, 1920, 1080), None);
    }

    #[tokio::test]
    async fn stream_start_passes_request() {
        let fake = Arc::new(Fake::default());
        let body = r#"{"url":"rtmp://a.rtmp.youtube.com/live2","key":"k-1","remember":false}"#;
        let r = app(fake.clone())
            .oneshot(req(
                "POST",
                "/api/stream/start",
                "127.0.0.1:8765",
                Some(TOKEN),
                Some(body),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(fake.streams.lock().unwrap()[0].key, "k-1");
    }

    async fn input_post(fake: Arc<Fake>, body: &str) -> (StatusCode, String) {
        let r = app(fake)
            .oneshot(req(
                "POST",
                "/api/input",
                "127.0.0.1:8765",
                Some(TOKEN),
                Some(body),
            ))
            .await
            .unwrap();
        (r.status(), body_string(r).await)
    }

    #[tokio::test]
    async fn input_route_maps_output_coords_to_page_px() {
        let fake = Arc::new(Fake::default());
        let (code, _) = input_post(fake.clone(), r#"{"kind":"click","x":0.5,"y":0.5}"#).await;
        assert_eq!(code, StatusCode::OK);
        let (code, _) = input_post(
            fake.clone(),
            r#"{"kind":"scroll","x":0.25,"y":0.5,"dx":0,"dy":-120}"#,
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        let (code, _) = input_post(fake.clone(), r#"{"kind":"key","key":"Enter"}"#).await;
        assert_eq!(code, StatusCode::OK);
        let (code, _) = input_post(fake.clone(), r#"{"kind":"text","text":"hi"}"#).await;
        assert_eq!(code, StatusCode::OK);
        let ev = fake.events.lock().unwrap().clone();
        assert_eq!(
            ev[0],
            InputEvent::Move {
                x: 960,
                y: 540,
                mods: 0
            }
        );
        assert_eq!(
            ev[1],
            InputEvent::Button {
                x: 960,
                y: 540,
                button: MouseBtn::Left,
                down: true,
                clicks: 1,
                mods: 0
            }
        );
        assert!(matches!(
            ev[2],
            InputEvent::Button {
                down: false,
                x: 960,
                y: 540,
                ..
            }
        ));
        assert_eq!(
            ev[3],
            InputEvent::Wheel {
                x: 480,
                y: 540,
                dx: 0,
                dy: -120,
                mods: 0
            }
        );
        // Enter: down, char, up. "hi": two chars.
        assert!(matches!(
            ev[4],
            InputEvent::Key {
                action: KeyAction::Down,
                code: 13,
                ..
            }
        ));
        assert!(matches!(
            ev[5],
            InputEvent::Key {
                action: KeyAction::Char,
                ch: 13,
                ..
            }
        ));
        assert!(matches!(
            ev[6],
            InputEvent::Key {
                action: KeyAction::Up,
                code: 13,
                ..
            }
        ));
        assert!(matches!(
            ev[7],
            InputEvent::Key {
                action: KeyAction::Char,
                ch: 104,
                ..
            }
        ));
        assert_eq!(ev.len(), 9);
    }

    #[tokio::test]
    async fn input_route_rejects_out_of_range() {
        let fake = Arc::new(Fake::default());
        for body in [
            r#"{"kind":"click","x":1.5,"y":0.5}"#,
            r#"{"kind":"move","x":0.5,"y":-0.1}"#,
            r#"{"kind":"click","y":0.5}"#,
            r#"{"kind":"key","key":"Nope"}"#,
            r#"{"kind":"fly","x":0.5,"y":0.5}"#,
        ] {
            let (code, text) = input_post(fake.clone(), body).await;
            assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
            assert!(text.contains("\"error\""), "{text}");
        }
        let long = format!(r#"{{"kind":"text","text":"{}"}}"#, "a".repeat(10_001));
        let (code, _) = input_post(fake.clone(), &long).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert!(fake.events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn snapshot_route_returns_jpeg_or_503() {
        let fake = Arc::new(Fake::default());
        let get = |fake: Arc<Fake>| async move {
            app(fake)
                .oneshot(req(
                    "GET",
                    "/api/snapshot",
                    "127.0.0.1:8765",
                    Some(TOKEN),
                    None,
                ))
                .await
                .unwrap()
        };
        assert_eq!(
            get(fake.clone()).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        *fake.snap.lock().unwrap() = Some(vec![0xff, 0xd8, 0xff, 0xd9]);
        let r = get(fake).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()["content-type"], "image/jpeg");
        assert_eq!(
            r.into_body().collect().await.unwrap().to_bytes().as_ref(),
            [0xff, 0xd8, 0xff, 0xd9]
        );
    }

    #[test]
    fn snapshot_encoder_makes_1280x720_jpeg() {
        let f = p2c_core::Frame::new_black(1920, 1080);
        let jpg = crate::preview::encode_snapshot(&f).unwrap();
        assert_eq!(&jpg[..2], &[0xff, 0xd8]);
        // SOF0 marker holds height then width.
        let i = jpg.windows(2).position(|w| w == [0xff, 0xc0]).unwrap();
        let h = u16::from_be_bytes([jpg[i + 5], jpg[i + 6]]);
        let w = u16::from_be_bytes([jpg[i + 7], jpg[i + 8]]);
        assert_eq!((w, h), (1280, 720));
    }

    #[tokio::test]
    async fn agents_route_without_tray_is_503() {
        for method in ["GET", "POST"] {
            let r = app(Arc::default())
                .oneshot(req(
                    method,
                    "/api/agents",
                    "127.0.0.1:8765",
                    Some(TOKEN),
                    (method == "POST").then_some(r#"{"allowed":true}"#),
                ))
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(body_string(r).await, r#"{"error":"tray not available"}"#);
        }
    }

    #[tokio::test]
    async fn new_routes_require_token_and_host() {
        let fake = Arc::new(Fake::default());
        *fake.snap.lock().unwrap() = Some(vec![1]);
        for (method, uri, body) in [
            (
                "POST",
                "/api/input",
                Some(r#"{"kind":"click","x":0.5,"y":0.5}"#),
            ),
            ("GET", "/api/snapshot", None),
            ("GET", "/api/agents", None),
            ("POST", "/api/agents", Some(r#"{"allowed":true}"#)),
        ] {
            for (host, token) in [
                ("127.0.0.1:8765", None),
                ("127.0.0.1:8765", Some("wrong")),
                ("evil.test:8765", Some(TOKEN)),
            ] {
                let r = app(fake.clone())
                    .oneshot(req(method, uri, host, token, body))
                    .await
                    .unwrap();
                assert_eq!(r.status(), StatusCode::FORBIDDEN, "{method} {uri} {host}");
            }
        }
        assert!(fake.events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn input_maps_at_non_default_state_size() {
        // The page is rendered at the output size (never letterboxed), so
        // output 0..1 maps straight to page px at whatever size the app has.
        let fake = Arc::new(Fake::default());
        let a = router(State::new(fake.clone(), TOKEN.into(), PORT, 1280, 720));
        let r = a
            .oneshot(req(
                "POST",
                "/api/input",
                "127.0.0.1:8765",
                Some(TOKEN),
                Some(r#"{"kind":"move","x":1.0,"y":0.5}"#),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            fake.events.lock().unwrap()[0],
            InputEvent::Move {
                x: 1279,
                y: 360,
                mods: 0
            }
        );
    }

    #[tokio::test]
    async fn space_sends_char_and_newline_text_is_enter() {
        let fake = Arc::new(Fake::default());
        input_post(fake.clone(), r#"{"kind":"key","key":"Space"}"#).await;
        input_post(fake.clone(), r#"{"kind":"text","text":"a\r\nb\n"}"#).await;
        let ev = fake.events.lock().unwrap().clone();
        let acts: Vec<(KeyAction, i32, u16)> = ev
            .iter()
            .map(|e| match e {
                InputEvent::Key {
                    action, code, ch, ..
                } => (*action, *code, *ch),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            acts,
            vec![
                (KeyAction::Down, 32, 0),
                (KeyAction::Char, 32, 32),
                (KeyAction::Up, 32, 0),
                (KeyAction::Char, 0, 97),
                (KeyAction::Char, 0, 13),
                (KeyAction::Char, 0, 98),
                (KeyAction::Char, 0, 13),
            ]
        );
    }

    #[tokio::test]
    async fn oversize_body_is_413() {
        let big = format!(
            r#"{{"kind":"text","text":"{}"}}"#,
            "a".repeat(3 * 1024 * 1024)
        );
        let r = app(Arc::default())
            .oneshot(req(
                "POST",
                "/api/input",
                "127.0.0.1:8765",
                Some(TOKEN),
                Some(&big),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// The router and its per-launch background id.
    fn app_with_id(fake: Arc<Fake>) -> (axum::Router, String) {
        let st = State::new(fake, TOKEN.into(), PORT, 1920, 1080);
        let id = st.bg_id.to_string();
        (router(st), id)
    }

    #[tokio::test]
    async fn bg_route_needs_id_and_host() {
        let fake = Arc::new(Fake::default());
        *fake.page.lock().unwrap() = Some("<!doctype html><p>bg</p>".into());
        *fake.full_image.lock().unwrap() = Some(vec![0xff, 0xd8, 0xff, 0xd9]);
        let (a, id) = app_with_id(fake.clone());
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        // Right id and host, no token: served, never cached, locked down.
        let r = a
            .clone()
            .oneshot(req(
                "GET",
                &format!("/bg/{id}/current.html"),
                "127.0.0.1:8765",
                None,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()["cache-control"], "no-store");
        let csp = r.headers()["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("default-src 'none'"), "{csp}");
        assert!(
            csp.contains("frame-src https://www.youtube-nocookie.com"),
            "{csp}"
        );
        assert!(body_string(r).await.contains("<p>bg</p>"));
        let r = a
            .clone()
            .oneshot(req(
                "GET",
                &format!("/bg/{id}/image?v=3"),
                "localhost:8765",
                None,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()["content-type"], "image/jpeg");
        // Wrong id or foreign host: nothing.
        let wrong = "0".repeat(32);
        for (uri, host, want) in [
            (
                format!("/bg/{wrong}/current.html"),
                "127.0.0.1:8765",
                StatusCode::NOT_FOUND,
            ),
            (
                format!("/bg/{wrong}/image"),
                "127.0.0.1:8765",
                StatusCode::NOT_FOUND,
            ),
            (
                "/bg//current.html".to_string(),
                "127.0.0.1:8765",
                StatusCode::NOT_FOUND,
            ),
            (
                format!("/bg/{id}/other"),
                "127.0.0.1:8765",
                StatusCode::NOT_FOUND,
            ),
            (
                format!("/bg/{id}/current.html"),
                "evil.test:8765",
                StatusCode::FORBIDDEN,
            ),
            (
                format!("/bg/{id}/image"),
                "127.0.0.1:9999",
                StatusCode::FORBIDDEN,
            ),
        ] {
            let r = a
                .clone()
                .oneshot(req("GET", &uri, host, None, None))
                .await
                .unwrap();
            assert_eq!(r.status(), want, "{uri} {host}");
        }
        // The id opens only the background, never the API.
        for uri in [
            format!("/bg/{id}/../api/state"),
            format!("/api/state?bg={id}"),
        ] {
            let r = a
                .clone()
                .oneshot(req("GET", &uri, "127.0.0.1:8765", None, None))
                .await
                .unwrap();
            assert_ne!(r.status(), StatusCode::OK, "{uri}");
        }
        // No background page right now: 404.
        *fake.page.lock().unwrap() = None;
        let r = a
            .oneshot(req(
                "GET",
                &format!("/bg/{id}/current.html"),
                "127.0.0.1:8765",
                None,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }

    fn upload(uri: &str, body: Vec<u8>) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("host", "127.0.0.1:8765")
            .header("x-p2c-token", TOKEN)
            .header("content-type", "application/octet-stream")
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn image_upload_limits() {
        let fake = Arc::new(Fake::default());
        let png = |n: usize| {
            let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
            v.resize(n, 0);
            v
        };
        // 25 MB: 413, never reaches the app.
        let r = app(fake.clone())
            .oneshot(upload(
                "/api/background/image?row=full",
                png(25 * 1024 * 1024),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
        // HTML (named .png by the browser or not): 415.
        let r = app(fake.clone())
            .oneshot(upload(
                "/api/background/image?row=box",
                b"<!doctype html><script>alert(1)</script>".to_vec(),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(body_string(r).await.contains("\"error\""));
        assert!(fake.uploads.lock().unwrap().is_empty());
        // A 3 MB PNG (over the 2 MB limit of other routes) gets through.
        let r = app(fake.clone())
            .oneshot(upload(
                "/api/background/image?row=box",
                png(3 * 1024 * 1024),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        // JPEG and WebP too; unknown rows are refused.
        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        webp.resize(64, 0);
        for (uri, body, want) in [
            (
                "/api/background/image?row=full",
                vec![0xff, 0xd8, 0xff, 0xe0],
                StatusCode::OK,
            ),
            ("/api/background/image?row=full", webp, StatusCode::OK),
            (
                "/api/background/image?row=side",
                png(64),
                StatusCode::BAD_REQUEST,
            ),
            ("/api/background/image", png(64), StatusCode::BAD_REQUEST),
        ] {
            let r = app(fake.clone()).oneshot(upload(uri, body)).await.unwrap();
            assert_eq!(r.status(), want, "{uri}");
        }
        assert_eq!(
            *fake.uploads.lock().unwrap(),
            vec![
                (ImageRow::Box, 3 * 1024 * 1024),
                (ImageRow::Full, 4),
                (ImageRow::Full, 64)
            ]
        );
    }

    #[tokio::test]
    async fn background_route_round_trip() {
        let fake = Arc::new(Fake::default());
        let body =
            r##"{"full":"colour","full_colour":"#112233","in_box":"blur","refined_edges":true}"##;
        assert_eq!(
            post(fake.clone(), "/api/background", body).await,
            StatusCode::OK
        );
        let r = app(fake.clone())
            .oneshot(req(
                "GET",
                "/api/background",
                "127.0.0.1:8765",
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        assert_eq!(v["full"], "colour");
        assert_eq!(v["full_colour"], "#112233");
        assert_eq!(v["in_box"], "blur");
        assert_eq!(v["refined_edges"], true);
        // A bad link: 400 with the message, nothing changed.
        let r = app(fake.clone())
            .oneshot(req(
                "POST",
                "/api/background",
                "127.0.0.1:8765",
                Some(TOKEN),
                Some(r#"{"full":"youtube","full_youtube":"hello","in_box":"room"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert!(body_string(r).await.contains("not a YouTube video link"));
        assert_eq!(fake.background().full, "colour");
        // Status carries the background fields.
        let r = app(fake.clone())
            .oneshot(req(
                "GET",
                "/api/state",
                "127.0.0.1:8765",
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        for k in [
            "background",
            "box_background",
            "refined_edges",
            "background_note",
        ] {
            assert!(v["status"].get(k).is_some(), "missing {k}");
        }
    }

    #[tokio::test]
    async fn thumb_route_serves_the_engine_fetched_jpeg() {
        let fake = Arc::new(Fake::default());
        let r = app(fake.clone())
            .oneshot(req(
                "GET",
                "/api/background/thumb?link=https%3A%2F%2Fyoutu.be%2FdQw4w9WgXcQ",
                "127.0.0.1:8765",
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()["content-type"], "image/jpeg");
        let r = app(fake.clone())
            .oneshot(req(
                "GET",
                "/api/background/thumb",
                "127.0.0.1:8765",
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            *fake.thumbs.lock().unwrap(),
            vec![Some("https://youtu.be/dQw4w9WgXcQ".to_string()), None]
        );
        let r = app(fake.clone())
            .oneshot(req(
                "GET",
                "/api/background/thumb?link=slow",
                "127.0.0.1:8765",
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn background_routes_require_token_and_host() {
        let fake = Arc::new(Fake::default());
        for (method, uri, body) in [
            ("GET", "/api/background", None),
            (
                "POST",
                "/api/background",
                Some(r#"{"full":"page","in_box":"room"}"#),
            ),
            ("POST", "/api/background/image?row=full", Some("x")),
            ("GET", "/api/background/image?row=full", None),
            ("GET", "/api/background/thumb", None),
        ] {
            for (host, token) in [
                ("127.0.0.1:8765", None),
                ("127.0.0.1:8765", Some("wrong")),
                ("evil.test:8765", Some(TOKEN)),
            ] {
                let r = app(fake.clone())
                    .oneshot(req(method, uri, host, token, body))
                    .await
                    .unwrap();
                assert_eq!(r.status(), StatusCode::FORBIDDEN, "{method} {uri} {host}");
            }
        }
        assert!(fake.background.lock().unwrap().is_none());
        assert!(fake.uploads.lock().unwrap().is_empty());
        assert!(fake.thumbs.lock().unwrap().is_empty());
    }
}
