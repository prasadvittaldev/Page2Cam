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
            C::StartStream => {
                let st = self.state()?;
                let url = st["status"]["stream_url"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                if url.is_empty() || st["status"]["has_saved_key"] != json!(true) {
                    return Err("No saved stream destination and key: save them once in the Page2Cam control page".into());
                }
                self.post(
                    "/api/stream/start",
                    &json!({ "url": url, "key": "", "remember": false }),
                )?;
                Ok(BackendReply::Done("stream starting".into()))
            }
            C::StopStream => {
                self.post("/api/stream/stop", &json!({}))?;
                Ok(BackendReply::Done("stream stopped".into()))
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
            C::SetBackground(partial) => {
                let cur = self.get("/api/background")?;
                let new = Self::merged(&cur, partial);
                self.json("POST", "/api/background", Some(&new), SLOW_TIMEOUT)
                    .map(BackendReply::Json)
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
        }
    }
}

/// The overlay as agents see it (style names with `_`).
fn overlay_out(pip: &Value) -> Value {
    let mut v = pip.clone();
    if let Some(s) = v["style"].as_str().map(|s| s.replace('-', "_")) {
        v["style"] = s.into();
    }
    v
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
    fn stream_uses_the_saved_destination_and_never_a_key() {
        let (port, log) = fake_engine(|m, p, _| match (m, p) {
            ("GET", "/api/state") => (200, state_json()),
            _ => (200, Vec::new()),
        });
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "running".into();
        *tray.engine.lock() = Some((port, "tok".into()));
        let d = tempfile::tempdir().unwrap();
        let b = backend(tray.clone(), d.path());
        b.call("Claude", BackendCall::StartStream).unwrap();
        let log = log.lock();
        let r = log.iter().find(|r| r.1 == "/api/stream/start").unwrap();
        let body: Value = serde_json::from_slice(&r.2).unwrap();
        assert_eq!(
            body,
            json!({ "url": "rtmp://a.rtmp.youtube.com/live2", "key": "", "remember": false })
        );
        assert_eq!(
            tray.balloons.lock().as_slice(),
            ["Agent “Claude” started your stream"]
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
}
