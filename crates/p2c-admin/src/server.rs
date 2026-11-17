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
use p2c_core::FrameSlot;
use serde::Deserialize;
use tokio::sync::watch;

use crate::api::{
    AudioSettings, Controls, InputEvent, KeyAction, MouseBtn, NavAction, PipDto, StreamRequest,
};
use crate::input::{map_preview, normalize_url};
use crate::preview::spawn_encoder;

const INDEX_HTML: &str = include_str!("../ui/index.html");

type Jpeg = Option<Arc<Vec<u8>>>;

pub struct AdminConfig {
    /// Preferred port; falls back to any free port if taken.
    pub port: u16,
    pub width: u32,
    pub height: u32,
    pub open_browser: bool,
}

pub struct AdminServer {
    pub addr: SocketAddr,
    /// Full URL including the access token.
    pub url: String,
}

#[derive(Clone)]
pub(crate) struct State {
    controls: Arc<dyn Controls>,
    token: Arc<str>,
    port: u16,
    width: u32,
    height: u32,
    preview: Arc<watch::Sender<Jpeg>>,
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
        }
    }
}

pub(crate) fn router(st: State) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/state", get(state))
        .route("/api/navigate", post(navigate))
        .route("/api/nav", post(nav))
        .route("/api/pip", post(pip))
        .route("/api/stream/start", post(stream_start))
        .route("/api/stream/stop", post(stream_stop))
        .route("/api/audio", post(audio))
        .route("/ws", get(ws))
        .layer(middleware::from_fn_with_state(st.clone(), guard))
        .with_state(st)
}

/// Start the admin server on its own runtime thread. Returns once listening.
pub fn serve(
    cfg: AdminConfig,
    controls: Arc<dyn Controls>,
    preview_frames: FrameSlot,
) -> anyhow::Result<AdminServer> {
    let token = random_token();
    let (tx, rx) = std::sync::mpsc::channel::<anyhow::Result<SocketAddr>>();
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
                let _ = tx.send(Ok(addr));
                if let Err(e) = axum::serve(listener, router(st)).await {
                    tracing::error!(error = %e, "admin server stopped");
                }
            });
        })?;
    let addr = rx.recv()??;
    let url = format!("http://127.0.0.1:{}/?t={}", addr.port(), token);
    if cfg.open_browser {
        if let Err(e) = webbrowser::open(&url) {
            tracing::warn!(error = %e, "could not open a browser; open the admin URL manually");
        }
    }
    Ok(AdminServer { addr, url })
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

async fn index() -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-store")], Html(INDEX_HTML))
}

async fn state(AxState(st): AxState<State>) -> impl IntoResponse {
    let c = &st.controls;
    Json(serde_json::json!({
        "status": c.status(),
        "pip": PipDto::from(c.pip()),
        "audio": c.audio(),
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

async fn pip(AxState(st): AxState<State>, Json(b): Json<PipDto>) -> StatusCode {
    st.controls.set_pip(b.into());
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
    use p2c_compositor::PipLayout;
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
        }

        impl Controls for Fake {
            fn navigate(&self, url: &str) {
                self.navigated.lock().unwrap().push(url.into());
            }
            fn nav(&self, _a: NavAction) {}
            fn input(&self, _ev: InputEvent) {}
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
                    ..Status::default()
                }
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
                width: 0.3
            }
        );
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
}
