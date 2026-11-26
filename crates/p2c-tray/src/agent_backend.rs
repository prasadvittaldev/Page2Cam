//! Carries out agent calls: tray-side ones (start, stop, pages, update)
//! through [`TrayControl`], the rest through the engine's admin API. Records
//! the activity list and asks for balloons. Platform-free.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use p2c_mcp::{Backend, BackendCall, BackendReply};
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::agents::{balloon_for, file_url, list_pages, write_page, Activity, ActivityEntry};

/// What the tray does for agent calls (the Windows controller, or a fake).
pub trait TrayControl: Send + Sync + 'static {
    /// The engine's admin port and token while it is running.
    fn engine(&self) -> Option<(u16, String)>;
    /// "stopped" | "starting" | "running" | "stopping" | "error: <why>"
    fn state(&self) -> String;
    fn start(&self);
    fn stop(&self);
    fn force_restart(&self);
    /// Start installing the update on offer; Err when none is offered.
    fn install_update(&self) -> Result<String, String>;
    fn balloon(&self, text: String);
    /// An agent made a call (icon badge, tooltip line).
    fn agent_active(&self, name: &str);
}

/// How long engine calls may take (picture uploads and background changes
/// decode images and write files).
const ENGINE_TIMEOUT: Duration = Duration::from_secs(5);
const SLOW_TIMEOUT: Duration = Duration::from_secs(30);
const START_WAIT: Duration = Duration::from_secs(40);
const STOP_WAIT: Duration = Duration::from_secs(20);
const MAX_ANSWER: u64 = 8 * 1024 * 1024;

pub struct TrayBackend {
    ctl: Arc<dyn TrayControl>,
    /// `Documents\Page2Cam\pages`, or why it can't be found (pages refused).
    pages_dir: Result<PathBuf, String>,
    activity: Arc<Mutex<Activity>>,
    /// How often start/stop re-check the state (tests make it short).
    pub poll: Duration,
}

impl TrayBackend {
    pub fn new(
        ctl: Arc<dyn TrayControl>,
        pages_dir: Result<PathBuf, String>,
        activity: Arc<Mutex<Activity>>,
    ) -> Self {
        TrayBackend {
            ctl,
            pages_dir,
            activity,
            poll: Duration::from_millis(250),
        }
    }

    fn engine(&self) -> Result<(u16, String), String> {
        self.ctl
            .engine()
            .ok_or_else(|| p2c_mcp::STOPPED.to_string())
    }

    fn http(
        &self,
        method: &str,
        path: &str,
        body: Option<(&str, &[u8])>,
        timeout: Duration,
    ) -> Result<(u16, Vec<u8>), String> {
        let (port, token) = self.engine()?;
        crate::http::request_full(port, method, path, &token, body, timeout, MAX_ANSWER)
            .map_err(|e| format!("Page2Cam did not answer: {e:#}"))
    }

    /// A JSON call that must answer 200; returns its JSON (or `null`).
    fn json(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Value, String> {
        let b = body.map(|v| v.to_string());
        let (code, bytes) = self.http(
            method,
            path,
            b.as_deref().map(|s| ("application/json", s.as_bytes())),
            timeout,
        )?;
        if code != 200 {
            return Err(error_text(code, &bytes));
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        self.json("GET", path, None, ENGINE_TIMEOUT)
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value, String> {
        self.json("POST", path, Some(body), ENGINE_TIMEOUT)
    }

    fn state(&self) -> Result<Value, String> {
        self.get("/api/state")
    }

    /// `current` with the fields of `partial` replaced.
    fn merged(current: &Value, partial: &Value) -> Value {
        let mut m = current.as_object().cloned().unwrap_or_default();
        if let Some(p) = partial.as_object() {
            for (k, v) in p {
                m.insert(k.clone(), v.clone());
            }
        }
        Value::Object(m)
    }

    fn wait_for(&self, limit: Duration, done: impl Fn(&str) -> bool) -> Result<String, String> {
        let until = Instant::now() + limit;
        loop {
            let s = self.ctl.state();
            if done(&s) {
                return Ok(s);
            }
            if let Some(why) = s.strip_prefix("error: ") {
                return Err(format!("Page2Cam could not start: {why}"));
            }
            if Instant::now() >= until {
                return Err(format!("Page2Cam is still {s} after {} s", limit.as_secs()));
            }
            std::thread::sleep(self.poll);
        }
    }

    fn run(&self, call: &BackendCall) -> Result<BackendReply, String> {
        use BackendCall as C;
        match call {
            C::Status => {
                let tray = self.ctl.state();
                if self.ctl.engine().is_none() {
                    return Ok(BackendReply::Json(
                        json!({ "state": tray, "running": false }),
                    ));
                }
                let mut v = self.state()?;
                if let Some(m) = v.as_object_mut() {
                    m.insert("state".into(), tray.into());
                    m.insert("running".into(), true.into());
                }
                Ok(BackendReply::Json(v))
            }
            C::Start => {
                if self.ctl.state() == "running" {
                    return Ok(BackendReply::Done("Page2Cam is already running".into()));
                }
                self.ctl.start();
                self.wait_for(START_WAIT, |s| s == "running")?;
                Ok(BackendReply::Done("Page2Cam is running".into()))
            }
            C::Stop => {
                if matches!(self.ctl.state().as_str(), "stopped") {
                    return Ok(BackendReply::Done("Page2Cam is already stopped".into()));
                }
                self.ctl.stop();
                self.wait_for(STOP_WAIT, |s| s == "stopped" || s.starts_with("error"))
                    .or_else(|e| {
                        if e.starts_with("Page2Cam could not start") {
                            Ok(String::new())
                        } else {
                            Err(e)
                        }
                    })?;
                Ok(BackendReply::Done("Page2Cam stopped".into()))
            }
            C::ForceRestart => {
                self.ctl.force_restart();
                // Give the supervisor a moment to leave Running.
                std::thread::sleep(self.poll);
                self.wait_for(START_WAIT, |s| s == "running")?;
                Ok(BackendReply::Done(
                    "Page2Cam restarted and is running".into(),
                ))
            }
            C::InstallUpdate => self.ctl.install_update().map(BackendReply::Done),
            C::ListPages => {
                let dir = self.pages_dir.as_ref().map_err(Clone::clone)?;
                Ok(BackendReply::Json(json!({
                    "folder": dir.display().to_string(),
                    "pages": list_pages(dir),
                })))
            }
            C::ShowPage { name, html } => {
                let dir = self.pages_dir.as_ref().map_err(Clone::clone)?;
                self.engine()?;
                let p = write_page(dir, name, html)
                    .map_err(|e| format!("could not save the page: {e}"))?;
                let url = file_url(&p);
                self.post("/api/navigate", &json!({ "url": url }))?;
                Ok(BackendReply::Done(format!(
                    "saved {} and showing it",
                    p.display()
                )))
            }
            C::OpenUrl(u) => {
                let v = self.post("/api/navigate", &json!({ "url": u }))?;
                Ok(BackendReply::Done(format!(
                    "showing {}",
                    v["url"].as_str().unwrap_or(u)
                )))
            }
            C::Navigate(n) => {
                self.post("/api/nav", &json!({ "action": n.as_str() }))?;
                Ok(BackendReply::Done("ok".into()))
            }
            C::Input(body) => {
                self.post("/api/input", body)?;
                Ok(BackendReply::Done("ok".into()))
            }
            C::Snapshot => {
                let (code, bytes) = self.http("GET", "/api/snapshot", None, ENGINE_TIMEOUT)?;
                match code {
                    200 => Ok(BackendReply::Image(bytes)),
                    503 => Err("no picture yet; try again in a moment".into()),
                    _ => Err(error_text(code, &bytes)),
                }
            }
            C::GetOverlay => Ok(BackendReply::Json(overlay_out(&self.state()?["pip"]))),
            C::SetOverlay(partial) => {
                let cur = self.state()?["pip"].clone();
                let new = Self::merged(&cur, partial);
                self.post("/api/pip", &new)?;
                Ok(BackendReply::Json(overlay_out(&self.state()?["pip"])))
            }
            C::SetCutoutQuality(q) => {
                self.post("/api/cutout", &json!({ "engine": q }))?;
                Ok(BackendReply::Done(format!("cut-out set to {q}")))
            }
            C::GetCrop => self.get("/api/crop").map(BackendReply::Json),
            C::SetCrop(c) => {
                let body = Self::merged(c, &json!({ "save": true }));
                self.post("/api/crop", &body).map(BackendReply::Json)
            }
            C::ResetCrop => self
                .post(
                    "/api/crop",
                    &json!({ "x": 0.0, "y": 0.0, "w": 1.0, "h": 1.0, "save": true }),
                )
                .map(BackendReply::Json),
            C::StartStream { sites } => {
                // No keys, no urls: the engine uses the saved ones and answers
                // with its own message (e.g. "Twitch has no stream key").
                let body = match sites {
                    Some(list) => {
                        let on = |s: &str| list.iter().any(|x| x == s);
                        json!({ "sites": {
                            "youtube": { "on": on("youtube") },
                            "twitch": { "on": on("twitch") },
                        } })
                    }
                    None => json!({ "sites": {} }),
                };
                self.post("/api/stream/start", &body)?;
                Ok(BackendReply::Done("stream starting".into()))
            }
            C::StopStream { sites: Some(list) } => {
                let mut m = serde_json::Map::new();
                for s in list {
                    m.insert(s.clone(), json!({ "on": false }));
                }
                self.post("/api/stream/sites", &Value::Object(m))?;
                Ok(BackendReply::Done("stream sites stopped".into()))
            }
            C::StopStream { sites: None } => {
                self.post("/api/stream/stop", &json!({}))?;
                Ok(BackendReply::Done("stream stopped".into()))
            }
            C::SetStreamSites { youtube, twitch } => {
                let mut m = serde_json::Map::new();
                for (name, v) in [("youtube", youtube), ("twitch", twitch)] {
                    if let Some(on) = v {
                        m.insert(name.into(), json!({ "on": on }));
                    }
                }
                self.post("/api/stream/sites", &Value::Object(m))?;
                Ok(BackendReply::Done("stream sites updated".into()))
            }
            C::SetAudio(partial) => {
                let cur = self.state()?["audio"].clone();
                let new = Self::merged(&cur, partial);
                self.post("/api/audio", &new)?;
                Ok(BackendReply::Json(new))
            }
            C::ListGpus => {
                let list = self.get("/api/gpus")?;
                let st = self.state()?;
                Ok(BackendReply::Json(json!({
                    "choices": list,
                    "saved": st["status"]["gpu_choice"],
                    "running": st["status"]["gpu_running"],
                    "video": st["status"]["video"],
                })))
            }
            C::SetGpu(choice) => {
                self.post("/api/gpu", &json!({ "choice": choice }))?;
                if self.state()?["status"]["restart_needed"] == json!(true) {
                    self.post("/api/restart", &json!({}))?;
                    return Ok(BackendReply::Done(format!(
                        "GPU set to {choice}; Page2Cam is restarting"
                    )));
                }
                Ok(BackendReply::Done(format!(
                    "GPU set to {choice} (already in use)"
                )))
            }
            C::GetBackground => self.get("/api/background").map(BackendReply::Json),
            C::ListScreens => self.get("/api/screens").map(BackendReply::Json),
            C::ListWindows => self.get("/api/windows").map(BackendReply::Json),
            C::SetBackground(partial) => {
                let cur = self.get("/api/background")?;
                let new = Self::merged(&cur, partial);
                let mut out = self.json("POST", "/api/background", Some(&new), SLOW_TIMEOUT)?;
                // The engine's notes (a style switched with it, a browser
                // window's shared sound, a minimized window...) reach the
                // agent too: the window note separately, since the style
                // note hides it in `background_note`.
                if let Some(note) = self.state().ok().and_then(|s| {
                    let mut notes: Vec<&str> = Vec::new();
                    for k in ["background_note", "window_note"] {
                        if let Some(n) = s["status"][k].as_str().filter(|n| !n.is_empty()) {
                            if !notes.contains(&n) {
                                notes.push(n);
                            }
                        }
                    }
                    (!notes.is_empty()).then(|| notes.join("; "))
                }) {
                    if let Some(m) = out.as_object_mut() {
                        m.insert("note".into(), note.into());
                    }
                }
                Ok(BackendReply::Json(out))
            }
            C::UploadBackgroundImage { row, bytes } => {
                let (code, out) = self.http(
                    "POST",
                    &format!("/api/background/image?row={row}"),
                    Some(("application/octet-stream", bytes)),
                    SLOW_TIMEOUT,
                )?;
                if code != 200 {
                    return Err(error_text(code, &out));
                }
                Ok(BackendReply::Json(
                    serde_json::from_slice(&out).unwrap_or(Value::Null),
                ))
            }
            C::ListLayers => {
                let v = self.get("/api/layers")?;
                Ok(BackendReply::Json(layers_out(&v["layers"])))
            }
            // Media may be copied (videos up to 200 MB): the slow timeout.
            C::AddLayer(body) => self
                .json("POST", "/api/layers", Some(body), SLOW_TIMEOUT)
                .map(|l| BackendReply::Json(layer_out(&l))),
            // Partial: the engine merges it into the layer.
            C::UpdateLayer { id, patch } => self
                .json(
                    "POST",
                    &format!("/api/layers/{id}"),
                    Some(patch),
                    SLOW_TIMEOUT,
                )
                .map(|l| BackendReply::Json(layer_out(&l))),
            C::ShowLayer(id) | C::HideLayer(id) => {
                let visible = matches!(call, C::ShowLayer(_));
                self.post(&format!("/api/layers/{id}"), &json!({ "visible": visible }))?;
                Ok(BackendReply::Done(format!(
                    "layer {id} {}",
                    if visible { "shown" } else { "hidden" }
                )))
            }
            C::RemoveLayer(id) => {
                self.json("DELETE", &format!("/api/layers/{id}"), None, ENGINE_TIMEOUT)?;
                Ok(BackendReply::Done(format!("layer {id} removed")))
            }
            C::ClearLayers => {
                let v = self.post("/api/layers/clear", &json!({}))?;
                Ok(BackendReply::Done(format!(
                    "{} layer(s) hidden",
                    v["hidden"].as_u64().unwrap_or(0)
                )))
            }
            C::ReorderLayers(ids) => {
                let v = self.post("/api/layers/order", &json!({ "ids": ids }))?;
                Ok(BackendReply::Json(layers_out(&v["layers"])))
            }
            // The MCP server answers it itself; here for completeness.
            C::Guide(t) => p2c_guide::topic(t.as_deref().unwrap_or(""))
                .map(|(_, text)| BackendReply::Done(text)),
            C::ListTemplates(cat) => {
                if self.ctl.engine().is_none() {
                    let lib = p2c_overlay::TemplateLib::builtin_only();
                    let mut v = lib.list_json(cat.as_deref());
                    v["note"] = json!("Page2Cam is stopped: built-in templates only (start it to see the user's own)");
                    return Ok(BackendReply::Json(v));
                }
                let path = match cat {
                    Some(c) => format!("/api/templates?category={c}"),
                    None => "/api/templates".into(),
                };
                self.get(&path).map(BackendReply::Json)
            }
            C::GetTemplate { id, include_html } => {
                let mut v = if self.ctl.engine().is_none() {
                    p2c_overlay::builtin(id)
                        .map(|t| t.to_json(true))
                        .ok_or_else(|| {
                            format!(
                                "no built-in template {id:?}; start Page2Cam to see the user's own"
                            )
                        })?
                } else {
                    self.get(&format!("/api/templates/{id}"))?
                };
                if !include_html && v["builtin"] == json!(true) {
                    if let Some(m) = v.as_object_mut() {
                        if let Some(h) = m.remove("html") {
                            m.insert("html_bytes".into(), h.as_str().map_or(0, str::len).into());
                        }
                    }
                }
                Ok(BackendReply::Json(v))
            }
            C::SaveTemplate(body) => {
                let mut v = self.json("POST", "/api/templates", Some(body), SLOW_TIMEOUT)?;
                if let Some(m) = v.as_object_mut() {
                    if let Some(h) = m.remove("html") {
                        m.insert("html_bytes".into(), h.as_str().map_or(0, str::len).into());
                    }
                }
                Ok(BackendReply::Json(v))
            }
            C::DeleteTemplate(id) => {
                self.json(
                    "DELETE",
                    &format!("/api/templates/{id}"),
                    None,
                    ENGINE_TIMEOUT,
                )?;
                Ok(BackendReply::Done(format!("template {id} deleted")))
            }
            C::ListScenes => self.get("/api/scenes").map(BackendReply::Json),
            C::SaveScene {
                name,
                include_background,
                include_webcam,
            } => self
                .json(
                    "POST",
                    "/api/scenes",
                    Some(
                        &json!({ "name": name, "include_background": include_background,
                        "include_webcam": include_webcam }),
                    ),
                    SLOW_TIMEOUT,
                )
                .map(BackendReply::Json),
            C::LoadScene { id, replace } => self
                .json(
                    "POST",
                    &format!("/api/scenes/{id}/load"),
                    Some(&json!({ "mode": if *replace { "replace" } else { "add" } })),
                    SLOW_TIMEOUT,
                )
                .map(BackendReply::Json),
            C::DeleteScene(id) => {
                self.json("DELETE", &format!("/api/scenes/{id}"), None, ENGINE_TIMEOUT)?;
                Ok(BackendReply::Done(format!("scene {id} deleted")))
            }
        }
    }
}

/// One layer as agents see it: its fields without the bulky HTML, with a
/// short summary of the content.
fn layer_out(l: &Value) -> Value {
    let mut v = l.clone();
    if let Some(m) = v.as_object_mut() {
        if let Some(h) = m.remove("html") {
            let n = h.as_str().map_or(0, str::len);
            m.insert("html_bytes".into(), n.into());
        }
    }
    v
}

fn layers_out(list: &Value) -> Value {
    let layers: Vec<Value> = list
        .as_array()
        .map(|a| a.iter().map(layer_out).collect())
        .unwrap_or_default();
    json!({ "layers": layers })
}

/// The overlay as agents see it (style names with `_`).
fn overlay_out(pip: &Value) -> Value {
    let mut v = pip.clone();
    if let Some(s) = v["style"].as_str().map(|s| s.replace('-', "_")) {
        v["style"] = s.into();
    }
    v
}

/// The partial `set_background` names the window background (`full: window`
/// or a `full_window` value).
fn touches_window(p: &Value) -> bool {
    p["full"] == "window" || p.get("full_window").is_some()
}

/// The title part of the engine's window label "<title> (<exe>)": the
/// trailing " (xxx.exe)" is dropped, the rest (or the whole text when it has
/// no such suffix) is kept, one line and short.
fn window_title(label: &str) -> String {
    if label.trim().is_empty() {
        return "(untitled)".into();
    }
    let t = match label.rfind(" (") {
        Some(i) if label.ends_with(')') && label[i..].to_lowercase().ends_with(".exe)") => {
            &label[..i]
        }
        _ => label,
    };
    t.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(80)
        .collect()
}

/// The engine's error body (`{"error": …}` or text) as a message.
fn error_text(code: u16, body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let msg = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["error"].as_str().map(String::from))
        .unwrap_or_else(|| text.trim().chars().take(300).collect());
    if msg.is_empty() {
        format!("Page2Cam answered {code}")
    } else {
        msg
    }
}

impl Backend for TrayBackend {
    fn call(&self, agent: &str, req: BackendCall) -> Result<BackendReply, String> {
        self.ctl.agent_active(agent);
        let running = self.ctl.engine().is_some();
        // Stopping a live stream ends it: say so.
        let streaming = matches!(req, BackendCall::Stop)
            && running
            && self.state().is_ok_and(|s| {
                s["status"]["stream"]
                    .as_str()
                    .is_some_and(|x| x == "live" || x == "connecting")
            });
        let r = self.run(&req);
        if req.changes() {
            let result = match &r {
                Ok(_) => "ok".to_string(),
                Err(e) => p2c_mcp::redact(e).chars().take(200).collect(),
            };
            self.activity.lock().push(ActivityEntry {
                time: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                agent: agent.to_string(),
                action: req.summary(),
                result,
            });
        }
        if r.is_ok() {
            if let Some(mut text) = balloon_for(agent, &req, running) {
                if streaming {
                    text.push_str(" (stream ended)");
                }
                self.ctl.balloon(text);
            }
            // A shared window: said always, with the title the engine resolved.
            let short: String = agent.chars().take(40).collect();
            match (&req, &r) {
                // Decided from the engine's merged answer, so a partial that
                // only names `window` counts too (balloon_for stays quiet for
                // those; the generic balloon is sent here when it isn't window).
                (BackendCall::SetBackground(p), Ok(BackendReply::Json(v))) if touches_window(p) => {
                    if v["full"] == "window" {
                        self.ctl.balloon(format!(
                            "Agent \u{201c}{short}\u{201d} is sharing a window: {}",
                            window_title(v["full_window"].as_str().unwrap_or(""))
                        ));
                    } else if running {
                        self.ctl.balloon(format!(
                            "Agent \u{201c}{short}\u{201d} changed your background"
                        ));
                    }
                }
                (BackendCall::LoadScene { .. }, Ok(BackendReply::Json(v)))
                    if v["background_full"] == "window" =>
                {
                    // The scene answer has no title: ask for the current background.
                    let label = self
                        .get("/api/background")
                        .ok()
                        .and_then(|b| b["full_window"].as_str().map(String::from))
                        .unwrap_or_default();
                    self.ctl.balloon(format!(
                        "Agent \u{201c}{short}\u{201d} is sharing a window: {}",
                        window_title(&label)
                    ));
                }
                _ => {}
            }
            // A scene may bring back a screen-sharing background: said always.
            if let (BackendCall::LoadScene { .. }, Ok(BackendReply::Json(v))) = (&req, &r) {
                if v["background_full"] == "screen" {
                    let short: String = agent.chars().take(40).collect();
                    self.ctl.balloon(format!(
                        "Agent \u{201c}{short}\u{201d} is sharing your screen"
                    ));
                }
            }
        }
        r
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    type EngineAddr = Option<(u16, String)>;
    /// Requests the fake engine got: (method, path, body).
    pub type ReqLog = Arc<Mutex<Vec<(String, String, Vec<u8>)>>>;

    /// A fake tray: a state string and an optional engine port.
    #[derive(Default)]
    pub struct FakeTray {
        pub engine: Mutex<Option<(u16, String)>>,
        pub state: Mutex<String>,
        pub balloons: Mutex<Vec<String>>,
        pub started: Mutex<u32>,
        /// What `start` switches the state to.
        pub start_to: Mutex<Option<(String, EngineAddr)>>,
    }

    impl TrayControl for FakeTray {
        fn engine(&self) -> Option<(u16, String)> {
            self.engine.lock().clone()
        }
        fn state(&self) -> String {
            self.state.lock().clone()
        }
        fn start(&self) {
            *self.started.lock() += 1;
            if let Some((s, e)) = self.start_to.lock().clone() {
                *self.state.lock() = s;
                *self.engine.lock() = e;
            }
        }
        fn stop(&self) {
            *self.state.lock() = "stopped".into();
            *self.engine.lock() = None;
        }
        fn force_restart(&self) {}
        fn install_update(&self) -> Result<String, String> {
            Err("no update".into())
        }
        fn balloon(&self, text: String) {
            self.balloons.lock().push(text);
        }
        fn agent_active(&self, _: &str) {}
    }

    /// A fake engine: answers each request with `answer(method, path, body)`;
    /// records requests.
    pub fn fake_engine(
        answer: impl Fn(&str, &str, &[u8]) -> (u16, Vec<u8>) + Send + Sync + 'static,
    ) -> (u16, ReqLog) {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let answer = Arc::new(answer);
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let log = log2.clone();
                let answer = answer.clone();
                std::thread::spawn(move || {
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let path = parts.next().unwrap_or("").to_string();
                    let mut len = 0usize;
                    loop {
                        let mut h = String::new();
                        r.read_line(&mut h).unwrap();
                        if h == "\r\n" || h.is_empty() {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':') {
                            if k.eq_ignore_ascii_case("content-length") {
                                len = v.trim().parse().unwrap_or(0);
                            }
                        }
                    }
                    let mut body = vec![0; len];
                    r.read_exact(&mut body).unwrap();
                    let (code, out) = answer(&method, &path, &body);
                    log.lock().push((method, path, body));
                    let head =
                        format!("HTTP/1.1 {code} X\r\ncontent-length: {}\r\n\r\n", out.len());
                    let _ = s.write_all(head.as_bytes());
                    let _ = s.write_all(&out);
                    let _ = s.shutdown(std::net::Shutdown::Write);
                    let _ = std::io::copy(&mut r, &mut std::io::sink());
                });
            }
        });
        (port, log)
    }

    pub fn state_json() -> Vec<u8> {
        json!({
            "status": { "stream": "idle", "stream_url": "rtmp://a.rtmp.youtube.com/live2", "has_saved_key": true,
                "gpu_choice": "auto", "gpu_running": "auto", "restart_needed": false, "video": "GPU" },
            "pip": { "visible": true, "x": 1.0, "y": 1.0, "width": 0.25, "style": "cutout-full" },
            "audio": { "page_on": true, "page_gain": 1.0, "mic_on": false, "mic_gain": 1.0 },
            "crop": { "x": 0.0, "y": 0.0, "w": 1.0, "h": 1.0 }
        })
        .to_string()
        .into_bytes()
    }

    fn backend(tray: Arc<FakeTray>, dir: &std::path::Path) -> TrayBackend {
        let mut b = TrayBackend::new(
            tray,
            Ok(dir.to_path_buf()),
            Arc::new(Mutex::new(Activity::default())),
        );
        b.poll = Duration::from_millis(10);
        b
    }

    #[test]
    fn no_documents_folder_means_no_pages() {
        let (port, log) = fake_engine(|_, _, _| (200, b"{}".to_vec()));
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "tok".into()));
        let b = TrayBackend::new(
            tray,
            Err("the Documents folder could not be found".into()),
            Arc::new(Mutex::new(Activity::default())),
        );
        let e = b
            .call(
                "A",
                BackendCall::ShowPage {
                    name: "a".into(),
                    html: "<p>".into(),
                },
            )
            .unwrap_err();
        assert!(e.contains("Documents"), "{e}");
        assert!(b.call("A", BackendCall::ListPages).is_err());
        assert!(log.lock().is_empty(), "nothing shown");
    }

    #[test]
    fn engine_tools_when_stopped_fail_fast() {
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "stopped".into();
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray.clone(), d.path());
        let t = Instant::now();
        for c in [
            BackendCall::OpenUrl("https://x".into()),
            BackendCall::Snapshot,
            BackendCall::GetOverlay,
            BackendCall::SetBackground(json!({ "full": "page" })),
            BackendCall::ShowPage {
                name: "a".into(),
                html: "<p>".into(),
            },
        ] {
            let e = b.call("A", c.clone()).unwrap_err();
            assert_eq!(e, p2c_mcp::STOPPED, "{c:?}");
        }
        assert!(t.elapsed() < Duration::from_secs(5));
        // Nothing written while stopped.
        assert!(list_pages(d.path()).is_empty());
        // Status still works.
        match b.call("A", BackendCall::Status).unwrap() {
            BackendReply::Json(v) => assert_eq!(v["running"], false),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn engine_that_does_not_answer_fails_within_5_s() {
        // A listener that never answers.
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let _keep = std::thread::spawn(move || {
            let _conns: Vec<_> = l.incoming().take(2).collect();
            std::thread::sleep(Duration::from_secs(10));
        });
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "t".into()));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray, d.path());
        let t = Instant::now();
        let e = b.call("A", BackendCall::GetCrop).unwrap_err();
        assert!(
            t.elapsed() < Duration::from_millis(5500),
            "{:?}",
            t.elapsed()
        );
        assert!(e.contains("did not answer"), "{e}");
    }

    #[test]
    fn show_page_writes_then_opens() {
        let (port, log) = fake_engine(|_, _, _| (200, br#"{"url":"x"}"#.to_vec()));
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "tok".into()));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray.clone(), d.path());
        b.call(
            "Claude",
            BackendCall::ShowPage {
                name: "My Agenda".into(),
                html: "<h1>Hi</h1>".into(),
            },
        )
        .unwrap();
        let p = d.path().join("My Agenda.html");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "<h1>Hi</h1>");
        let log = log.lock();
        assert_eq!(log[0].0, "POST");
        assert_eq!(log[0].1, "/api/navigate");
        let body: Value = serde_json::from_slice(&log[0].2).unwrap();
        assert_eq!(body["url"], file_url(&p));
        assert_eq!(
            tray.balloons.lock().as_slice(),
            ["Agent “Claude” changed the page"]
        );
        assert_eq!(b.activity.lock().list()[0].agent, "Claude");
    }

    #[test]
    fn partial_settings_are_merged_with_current() {
        let (port, log) = fake_engine(|m, p, _| match (m, p) {
            ("GET", "/api/state") => (200, state_json()),
            ("GET", "/api/background") => (
                200,
                br#"{"full":"page","in_box":"room","refined_edges":false}"#.to_vec(),
            ),
            _ => (200, b"{}".to_vec()),
        });
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "tok".into()));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray, d.path());
        b.call("A", BackendCall::SetOverlay(json!({ "x": 0.0 })))
            .unwrap();
        b.call("A", BackendCall::SetAudio(json!({ "mic_on": true })))
            .unwrap();
        b.call("A", BackendCall::SetBackground(json!({ "in_box": "blur" })))
            .unwrap();
        let log = log.lock();
        let posted = |path: &str| -> Value {
            serde_json::from_slice(&log.iter().find(|r| r.0 == "POST" && r.1 == path).unwrap().2)
                .unwrap()
        };
        assert_eq!(
            posted("/api/pip"),
            json!({ "visible": true, "x": 0.0, "y": 1.0, "width": 0.25, "style": "cutout-full" })
        );
        assert_eq!(
            posted("/api/audio"),
            json!({ "page_on": true, "page_gain": 1.0, "mic_on": true, "mic_gain": 1.0 })
        );
        assert_eq!(
            posted("/api/background"),
            json!({ "full": "page", "in_box": "blur", "refined_edges": false })
        );
    }

    #[test]
    fn screens_are_listed_and_screen_sharing_is_announced() {
        let (port, log) = fake_engine(|m, p, _| {
            match (m, p) {
            ("GET", "/api/state") => (200, state_json()),
            ("GET", "/api/screens") => (200, br#"[{"id":"DISPLAY1","index":1,"name":"Screen 1","width":1920,"height":1200,"primary":true}]"#.to_vec()),
            ("GET", "/api/background") => (
                200,
                br#"{"full":"page","full_screen":"","in_box":"room","refined_edges":false}"#.to_vec(),
            ),
            _ => (200, b"{}".to_vec()),
        }
        });
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "tok".into()));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray.clone(), d.path());
        let r = b.call("Claude", BackendCall::ListScreens).unwrap();
        assert_eq!(
            r,
            BackendReply::Json(
                json!([{"id":"DISPLAY1","index":1,"name":"Screen 1","width":1920,"height":1200,"primary":true}])
            )
        );
        assert!(tray.balloons.lock().is_empty(), "listing is quiet");
        b.call(
            "Claude",
            BackendCall::SetBackground(json!({ "full": "screen", "full_screen": "DISPLAY1" })),
        )
        .unwrap();
        let log = log.lock();
        let body = &log
            .iter()
            .find(|r| r.0 == "POST" && r.1 == "/api/background")
            .unwrap()
            .2;
        assert_eq!(
            serde_json::from_slice::<Value>(body).unwrap(),
            json!({ "full": "screen", "full_screen": "DISPLAY1", "in_box": "room", "refined_edges": false })
        );
        assert_eq!(
            tray.balloons.lock().as_slice(),
            ["Agent “Claude” is sharing your screen"]
        );
    }

    #[test]
    fn window_sharing_is_announced() {
        let (port, log) = fake_engine(|m, p, body| {
            match (m, p) {
            ("GET", "/api/state") => (
                200,
                br#"{"status":{"background_note":"Style set to full-height cut-out: a full-frame background needs the person cut out","window_note":"Sound: everything playing in Google Chrome"}}"#
                    .to_vec(),
            ),
            ("GET", "/api/windows") => (
                200,
                br#"[{"id":"123","title":"Twitch","exe":"chrome.exe"}]"#.to_vec(),
            ),
            ("GET", "/api/background") => (
                200,
                br#"{"full":"page","full_window":"","in_box":"room"}"#.to_vec(),
            ),
            ("POST", "/api/background") => {
                if String::from_utf8_lossy(body).contains("nomatch") {
                    (
                        400,
                        br#"{"error":"no window matches \"nomatch\""}"#.to_vec(),
                    )
                } else {
                    (
                        200,
                        br#"{"full":"window","full_window":"Twitch - Home (chrome.exe)"}"#.to_vec(),
                    )
                }
            }
            _ => (200, b"{}".to_vec()),
        }
        });
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "tok".into()));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray.clone(), d.path());
        let r = b.call("Claude", BackendCall::ListWindows).unwrap();
        assert!(matches!(r, BackendReply::Json(ref v) if v[0]["title"] == "Twitch"));
        assert!(tray.balloons.lock().is_empty(), "listing is quiet");
        let r = b
            .call(
                "Claude",
                BackendCall::SetBackground(json!({ "full": "window", "full_window": "Twitch" })),
            )
            .unwrap();
        let BackendReply::Json(v) = r else { panic!() };
        assert_eq!(v["full_window"], "Twitch - Home (chrome.exe)");
        // The style note and the window note both reach the agent.
        assert_eq!(
            v["note"],
            "Style set to full-height cut-out: a full-frame background needs the person cut out; Sound: everything playing in Google Chrome"
        );
        assert_eq!(
            tray.balloons.lock().as_slice(),
            ["Agent “Claude” is sharing a window: Twitch - Home"]
        );
        assert!(log
            .lock()
            .iter()
            .any(|r| r.1 == "/api/background" && r.0 == "POST"));
        // An engine error reaches the agent intact, with no balloon.
        let e = b
            .call(
                "Claude",
                BackendCall::SetBackground(json!({ "full": "window", "full_window": "nomatch" })),
            )
            .unwrap_err();
        assert!(e.contains("no window matches"), "{e}");
        assert_eq!(tray.balloons.lock().len(), 1);
    }

    #[test]
    fn window_only_partial_and_scene_load_are_announced() {
        let bg_ok = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let bg = bg_ok.clone();
        let (port, _log) = fake_engine(move |m, p, body| match (m, p) {
            ("GET", "/api/state") => (200, br#"{"status":{}}"#.to_vec()),
            ("GET", "/api/background") => {
                if bg.load(std::sync::atomic::Ordering::SeqCst) {
                    (
                        200,
                        br#"{"full":"window","full_window":"Notes (notepad.exe)"}"#.to_vec(),
                    )
                } else {
                    (500, br#"{"error":"boom"}"#.to_vec())
                }
            }
            ("POST", "/api/background") => {
                if String::from_utf8_lossy(body).contains("blank") {
                    (200, br#"{"full":"window","full_window":""}"#.to_vec())
                } else {
                    (
                        200,
                        br#"{"full":"window","full_window":"Other (chrome.exe)"}"#.to_vec(),
                    )
                }
            }
            ("POST", "/api/scenes/s/load") => (
                200,
                br#"{"added":[],"skipped":[],"background_full":"window"}"#.to_vec(),
            ),
            _ => (200, b"{}".to_vec()),
        });
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "tok".into()));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray.clone(), d.path());
        // Only `window` given: still the sharing balloon, not the generic one.
        b.call(
            "Claude",
            BackendCall::SetBackground(json!({ "full_window": "Other" })),
        )
        .unwrap();
        // An empty resolved title gets a placeholder, never the agent's text.
        b.call(
            "Claude",
            BackendCall::SetBackground(json!({ "full": "window", "full_window": "blank-7" })),
        )
        .unwrap();
        let scene = || BackendCall::LoadScene {
            id: "s".into(),
            replace: true,
        };
        b.call("Claude", scene()).unwrap();
        bg_ok.store(false, std::sync::atomic::Ordering::SeqCst);
        b.call("Claude", scene()).unwrap();
        assert_eq!(
            tray.balloons.lock().as_slice(),
            [
                "Agent “Claude” is sharing a window: Other",
                "Agent “Claude” is sharing a window: (untitled)",
                "Agent “Claude” is sharing a window: Notes",
                "Agent “Claude” is sharing a window: (untitled)",
            ]
        );
    }

    #[test]
    fn window_title_drops_the_exe_suffix() {
        assert_eq!(window_title(""), "(untitled)");
        assert_eq!(window_title("Twitch - Home (chrome.exe)"), "Twitch - Home");
        assert_eq!(window_title("Notes (draft) (Notepad.EXE)"), "Notes (draft)");
        assert_eq!(window_title("Report (final)"), "Report (final)");
        assert_eq!(window_title("a\nb"), "a b");
    }

    #[test]
    fn start_stream_posts_sites() {
        let (port, log) = fake_engine(|m, p, _| match (m, p) {
            ("GET", "/api/state") => (200, state_json()),
            _ => (200, Vec::new()),
        });
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "tok".into()));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray.clone(), d.path());
        b.call(
            "Claude",
            BackendCall::StartStream {
                sites: Some(vec!["twitch".into()]),
            },
        )
        .unwrap();
        b.call("Claude", BackendCall::StartStream { sites: None })
            .unwrap();
        let log = log.lock();
        let bodies: Vec<Value> = log
            .iter()
            .filter(|r| r.1 == "/api/stream/start")
            .map(|r| serde_json::from_slice(&r.2).unwrap())
            .collect();
        assert_eq!(
            bodies,
            vec![
                json!({ "sites": { "youtube": { "on": false }, "twitch": { "on": true } } }),
                json!({ "sites": {} }),
            ]
        );
        assert_eq!(
            tray.balloons.lock().as_slice(),
            [
                "Agent “Claude” started your stream",
                "Agent “Claude” started your stream"
            ]
        );
    }

    #[test]
    fn missing_key_message_reaches_the_agent() {
        let (port, _log) = fake_engine(|m, p, _| match (m, p) {
            ("POST", "/api/stream/start") => (500, b"Twitch has no stream key".to_vec()),
            _ => (200, state_json()),
        });
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "tok".into()));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray, d.path());
        let e = b
            .call("Claude", BackendCall::StartStream { sites: None })
            .unwrap_err();
        assert!(e.contains("Twitch has no stream key"), "{e}");
    }

    #[test]
    fn stop_stream_sites_and_set_sites_post_to_sites() {
        let (port, log) = fake_engine(|_, _, _| (200, b"{}".to_vec()));
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "tok".into()));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray, d.path());
        b.call(
            "A",
            BackendCall::StopStream {
                sites: Some(vec!["twitch".into()]),
            },
        )
        .unwrap();
        b.call(
            "A",
            BackendCall::SetStreamSites {
                youtube: Some(true),
                twitch: None,
            },
        )
        .unwrap();
        b.call("A", BackendCall::StopStream { sites: None })
            .unwrap();
        let log = log.lock();
        let posts: Vec<(&str, Value)> = log
            .iter()
            .map(|r| {
                (
                    r.1.as_str(),
                    serde_json::from_slice(&r.2).unwrap_or(Value::Null),
                )
            })
            .collect();
        assert_eq!(
            posts,
            vec![
                ("/api/stream/sites", json!({ "twitch": { "on": false } })),
                ("/api/stream/sites", json!({ "youtube": { "on": true } })),
                ("/api/stream/stop", json!({})),
            ]
        );
    }

    #[test]
    fn start_waits_until_running() {
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "stopped".into();
        *tray.start_to.lock() = Some(("running".into(), Some((1, "t".into()))));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray.clone(), d.path());
        assert!(matches!(
            b.call("Claude", BackendCall::Start),
            Ok(BackendReply::Done(_))
        ));
        assert_eq!(*tray.started.lock(), 1);
        assert_eq!(
            tray.balloons.lock().as_slice(),
            ["Agent “Claude” started Page2Cam"]
        );
        // Already running: no second start, no balloon.
        b.call("Claude", BackendCall::Start).unwrap();
        assert_eq!(*tray.started.lock(), 1);
        // A failed start is reported.
        *tray.state.lock() = "stopped".into();
        *tray.engine.lock() = None;
        *tray.start_to.lock() = Some(("error: camera".into(), None));
        assert!(b
            .call("Claude", BackendCall::Start)
            .unwrap_err()
            .contains("camera"));
    }

    #[test]
    fn engine_errors_reach_the_agent() {
        let (port, _) = fake_engine(|_, _, _| {
            (
                400,
                br#"{"error":"only PNG, JPEG and WebP images are accepted"}"#.to_vec(),
            )
        });
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "tok".into()));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray, d.path());
        let e = b
            .call(
                "A",
                BackendCall::UploadBackgroundImage {
                    row: "full".into(),
                    bytes: b"GIF89a".to_vec(),
                },
            )
            .unwrap_err();
        assert_eq!(e, "only PNG, JPEG and WebP images are accepted");
        assert_eq!(b.activity.lock().list()[0].result, e);
    }

    #[test]
    fn templates_listed_while_stopped() {
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "stopped".into();
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray, d.path());
        match b
            .call("A", BackendCall::ListTemplates(Some("time".into())))
            .unwrap()
        {
            BackendReply::Json(v) => {
                assert!(!v["templates"].as_array().unwrap().is_empty());
                assert!(v["note"].as_str().unwrap().contains("stopped"));
            }
            r => panic!("{r:?}"),
        }
        let get = |include_html| {
            b.call(
                "A",
                BackendCall::GetTemplate {
                    id: "countdown".into(),
                    include_html,
                },
            )
        };
        match get(false).unwrap() {
            BackendReply::Json(v) => {
                assert!(v.get("html").is_none() && v["html_bytes"].as_u64().unwrap() > 0);
                assert!(v["all_params"].is_array());
            }
            r => panic!("{r:?}"),
        }
        match get(true).unwrap() {
            BackendReply::Json(v) => assert!(v["html"].is_string()),
            r => panic!("{r:?}"),
        }
        assert_eq!(
            b.call("A", BackendCall::ListScenes).unwrap_err(),
            p2c_mcp::STOPPED
        );
    }

    #[test]
    fn template_and_scene_calls_reach_the_engine() {
        let (port, log) = fake_engine(|m, p, _| match (m, p) {
            ("POST", "/api/templates") => (200, br#"{"id":"mine","html":"<b>x</b>"}"#.to_vec()),
            ("POST", "/api/scenes/intro/load") => (
                200,
                br#"{"added":["t"],"skipped":[],"background_full":"screen"}"#.to_vec(),
            ),
            ("GET", "/api/templates/mine") => (
                200,
                br#"{"id":"mine","builtin":false,"html":"<b>x</b>"}"#.to_vec(),
            ),
            _ => (200, br#"{"scenes":[]}"#.to_vec()),
        });
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "tok".into()));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray.clone(), d.path());
        match b
            .call(
                "Claude",
                BackendCall::SaveTemplate(json!({ "copy_of": "countdown", "id": "mine" })),
            )
            .unwrap()
        {
            BackendReply::Json(v) => assert_eq!(v["html_bytes"], 8),
            r => panic!("{r:?}"),
        }
        match b
            .call(
                "Claude",
                BackendCall::GetTemplate {
                    id: "mine".into(),
                    include_html: false,
                },
            )
            .unwrap()
        {
            BackendReply::Json(v) => {
                assert_eq!(v["html"], "<b>x</b>", "the user's own keep their HTML")
            }
            r => panic!("{r:?}"),
        }
        b.call("Claude", BackendCall::ListTemplates(None)).unwrap();
        b.call("Claude", BackendCall::DeleteTemplate("mine".into()))
            .unwrap();
        b.call(
            "Claude",
            BackendCall::SaveScene {
                name: "Intro".into(),
                include_background: true,
                include_webcam: false,
            },
        )
        .unwrap();
        b.call(
            "Claude",
            BackendCall::LoadScene {
                id: "intro".into(),
                replace: false,
            },
        )
        .unwrap();
        b.call("Claude", BackendCall::ListScenes).unwrap();
        b.call("Claude", BackendCall::DeleteScene("intro".into()))
            .unwrap();
        let log = log.lock();
        let seen: Vec<String> = log.iter().map(|(m, p, _)| format!("{m} {p}")).collect();
        assert_eq!(
            seen,
            [
                "POST /api/templates",
                "GET /api/templates/mine",
                "GET /api/templates",
                "DELETE /api/templates/mine",
                "POST /api/scenes",
                "POST /api/scenes/intro/load",
                "GET /api/scenes",
                "DELETE /api/scenes/intro",
            ]
        );
        let body = |i: usize| -> Value { serde_json::from_slice(&log[i].2).unwrap() };
        assert_eq!(
            body(4),
            json!({ "name": "Intro", "include_background": true, "include_webcam": false })
        );
        assert_eq!(body(5), json!({ "mode": "add" }));
        assert_eq!(
            tray.balloons.lock().as_slice(),
            ["Agent \u{201c}Claude\u{201d} is sharing your screen"]
        );
        assert_eq!(b.activity.lock().list().len(), 5, "changes are listed");
    }

    #[test]
    fn layer_calls_reach_the_engine_without_balloons() {
        let (port, log) = fake_engine(|m, p, _| match (m, p) {
            ("GET", "/api/layers") => (
                200,
                br#"{"layers":[{"id":"chart","kind":"html","html":"<b>x</b>"}],"presets":[]}"#
                    .to_vec(),
            ),
            ("POST", "/api/layers") => (200, br#"{"id":"title-1","kind":"title"}"#.to_vec()),
            ("POST", "/api/layers/clear") => (200, br#"{"hidden":2}"#.to_vec()),
            ("POST", "/api/layers/nope") => (400, br#"{"error":"no layer \"nope\""}"#.to_vec()),
            _ => (200, br#"{"layers":[]}"#.to_vec()),
        });
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "tok".into()));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray.clone(), d.path());
        match b.call("Claude", BackendCall::ListLayers).unwrap() {
            BackendReply::Json(v) => {
                assert!(v["layers"][0].get("html").is_none(), "{v}");
                assert_eq!(v["layers"][0]["html_bytes"], 8);
            }
            r => panic!("{r:?}"),
        }
        b.call(
            "Claude",
            BackendCall::AddLayer(json!({ "kind": "title", "text": "Hi" })),
        )
        .unwrap();
        b.call(
            "Claude",
            BackendCall::UpdateLayer {
                id: "title-1".into(),
                patch: json!({ "x": 0.2 }),
            },
        )
        .unwrap();
        b.call("Claude", BackendCall::HideLayer("title-1".into()))
            .unwrap();
        b.call("Claude", BackendCall::RemoveLayer("title-1".into()))
            .unwrap();
        match b.call("Claude", BackendCall::ClearLayers).unwrap() {
            BackendReply::Done(t) => assert_eq!(t, "2 layer(s) hidden"),
            r => panic!("{r:?}"),
        }
        b.call("Claude", BackendCall::ReorderLayers(vec!["a".into()]))
            .unwrap();
        let e = b
            .call("Claude", BackendCall::ShowLayer("nope".into()))
            .unwrap_err();
        assert_eq!(e, "no layer \"nope\"");
        let log = log.lock();
        let seen: Vec<(String, String)> =
            log.iter().map(|(m, p, _)| (m.clone(), p.clone())).collect();
        let want = [
            ("GET", "/api/layers"),
            ("POST", "/api/layers"),
            ("POST", "/api/layers/title-1"),
            ("POST", "/api/layers/title-1"),
            ("DELETE", "/api/layers/title-1"),
            ("POST", "/api/layers/clear"),
            ("POST", "/api/layers/order"),
            ("POST", "/api/layers/nope"),
        ];
        let want: Vec<(String, String)> = want
            .iter()
            .map(|(m, p)| (m.to_string(), p.to_string()))
            .collect();
        assert_eq!(seen, want);
        let body = |i: usize| -> Value { serde_json::from_slice(&log[i].2).unwrap() };
        // The partial change goes as it is (the engine merges it).
        assert_eq!(body(2), json!({ "x": 0.2 }));
        assert_eq!(body(3), json!({ "visible": false }));
        assert_eq!(body(6), json!({ "ids": ["a"] }));
        assert_eq!(body(7), json!({ "visible": true }));
        assert!(tray.balloons.lock().is_empty(), "no balloons for layers");
        let acts = b.activity.lock().list();
        assert_eq!(
            acts.len(),
            7,
            "every change is listed, the list call is not"
        );
    }
}
