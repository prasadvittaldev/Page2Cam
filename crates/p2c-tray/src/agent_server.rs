//! The tray's local agent endpoint on 127.0.0.1 (std threads, no async):
//! `/tray/agents` (tray token, always) for the engine's Agents panel, and
//! `/mcp` (agent token, only while agents are allowed) for MCP clients.
//! Platform-free; the Windows controller starts it and supplies the backend.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use p2c_mcp::{ClientInfo, Server};
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::agent_backend::TrayBackend;
use crate::agents::{remove_discovery, snippets, write_discovery, Activity, RegResult};

const MAX_HEAD: usize = 16 * 1024;
/// An MCP message may carry a 20 MB picture as base64 (~27 MB).
const MAX_MCP_BODY: usize = 32 * 1024 * 1024;
const MAX_TRAY_BODY: usize = 64 * 1024;
const MAX_CONNECTIONS: usize = 16;
const MAX_SESSIONS: usize = 64;
const IO_TIMEOUT: Duration = Duration::from_secs(60);

/// Saves `agents_allowed` and `agent_token`.
pub type SaveFn = Box<dyn Fn(bool, &str) + Send + Sync>;
/// Registers (true) or unregisters (false) with installed agents.
pub type Registrar = Arc<dyn Fn(bool) -> Vec<RegResult> + Send + Sync>;

/// What the server needs from its surroundings.
pub struct ServerDeps {
    /// Guards `/tray/*` (shared with the engine through its environment).
    pub tray_token: String,
    /// `agent.json` (None: not written, e.g. no LOCALAPPDATA).
    pub discovery: Option<PathBuf>,
    /// The exe for the snippets.
    pub exe: String,
    /// Saves `agents_allowed` and `agent_token` (tray.toml).
    pub save: SaveFn,
    /// Registers with installed agents (true) or removes us (false). Runs on
    /// a worker thread (it may run the `claude` CLI).
    pub registrar: Registrar,
}

struct Shared {
    allowed: bool,
    token: String,
    registered: Vec<RegResult>,
    /// Bumped by every switch: an older registration result is dropped.
    reg_gen: u64,
    sessions: VecDeque<(String, ClientInfo)>,
}

struct Inner {
    port: u16,
    deps: ServerDeps,
    shared: Mutex<Shared>,
    mcp: Server<TrayBackend>,
    activity: Arc<Mutex<Activity>>,
    stop: AtomicBool,
    active: AtomicUsize,
    session_seq: AtomicU64,
}

/// The running endpoint. Dropping it stops accepting and removes the
/// discovery file.
pub struct AgentServer {
    inner: Arc<Inner>,
}

/// A new random token (32 hex chars).
pub fn new_token() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..32)
        .map(|_| format!("{:x}", rng.random_range(0..16u8)))
        .collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl AgentServer {
    /// Listen on `preferred` (or any free port when taken) and serve.
    /// `allowed`/`token` come from tray.toml (a missing token is made).
    pub fn start(
        preferred: u16,
        allowed: bool,
        token: Option<String>,
        deps: ServerDeps,
        backend: TrayBackend,
        activity: Arc<Mutex<Activity>>,
    ) -> std::io::Result<AgentServer> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, preferred))
            .or_else(|_| TcpListener::bind((Ipv4Addr::LOCALHOST, 0)))?;
        let port = listener.local_addr()?.port();
        let token = match token {
            Some(t) => t,
            None => {
                let t = new_token();
                (deps.save)(allowed, &t);
                t
            }
        };
        let inner = Arc::new(Inner {
            port,
            deps,
            shared: Mutex::new(Shared {
                allowed,
                token,
                registered: Vec::new(),
                reg_gen: 0,
                sessions: VecDeque::new(),
            }),
            mcp: Server::new(backend),
            activity,
            stop: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            session_seq: AtomicU64::new(0),
        });
        let me = AgentServer { inner };
        if allowed {
            me.write_discovery();
            // Keep registrations pointing at this exe (it may have moved).
            me.register(true);
        }
        let inner = me.inner.clone();
        std::thread::Builder::new()
            .name("p2c-tray-agents".into())
            .spawn(move || accept_loop(listener, inner))?;
        tracing::info!(port, allowed, "agent endpoint listening");
        Ok(me)
    }

    pub fn port(&self) -> u16 {
        self.inner.port
    }

    /// `http://127.0.0.1:<port>` (the engine's `P2C_TRAY_URL`).
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.inner.port)
    }

    pub fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base_url())
    }

    pub fn allowed(&self) -> bool {
        self.inner.shared.lock().allowed
    }

    /// Switch agents on or off (saved; discovery file and registrations follow).
    pub fn set_allowed(&self, on: bool) {
        set_allowed(&self.inner, on);
    }

    /// New agent token (old one stops working); discovery rewritten.
    pub fn reset_token(&self) {
        reset_token(&self.inner);
    }

    /// The Agents panel's data.
    pub fn agents_json(&self) -> Value {
        agents_json(&self.inner)
    }

    fn write_discovery(&self) {
        write_disc(&self.inner);
    }

    fn register(&self, on: bool) {
        register(&self.inner, on);
    }
}

impl Drop for AgentServer {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Release);
        if let Some(p) = &self.inner.deps.discovery {
            let _ = remove_discovery(p);
        }
        // Wake the accept loop so it sees `stop`.
        let _ = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, self.inner.port)),
            Duration::from_millis(200),
        );
    }
}

fn write_disc(inner: &Inner) {
    let Some(p) = &inner.deps.discovery else {
        return;
    };
    let token = inner.shared.lock().token.clone();
    let url = format!("http://127.0.0.1:{}/mcp", inner.port);
    if let Err(e) = write_discovery(p, &url, &token) {
        tracing::warn!("cannot write {}: {e}", p.display());
    }
}

fn register(inner: &Arc<Inner>, on: bool) {
    let gen = {
        let mut s = inner.shared.lock();
        s.reg_gen += 1;
        s.reg_gen
    };
    let inner = inner.clone();
    let _ = std::thread::Builder::new()
        .name("p2c-tray-register".into())
        .spawn(move || {
            let r = (inner.deps.registrar)(on);
            for x in &r {
                match &x.result {
                    Ok(()) => tracing::info!(app = %x.app, on, "agent registration"),
                    Err(e) => tracing::warn!(app = %x.app, on, "agent registration: {e}"),
                }
            }
            let mut s = inner.shared.lock();
            if s.reg_gen == gen {
                s.registered = if on { r } else { Vec::new() };
            }
        });
}

fn set_allowed(inner: &Arc<Inner>, on: bool) {
    let token = {
        let mut s = inner.shared.lock();
        if s.allowed == on {
            return;
        }
        s.allowed = on;
        s.sessions.clear();
        s.token.clone()
    };
    tracing::info!(on, "allow agents");
    (inner.deps.save)(on, &token);
    if on {
        write_disc(inner);
    } else if let Some(p) = &inner.deps.discovery {
        let _ = remove_discovery(p);
    }
    register(inner, on);
}

fn reset_token(inner: &Arc<Inner>) {
    let (allowed, token) = {
        let mut s = inner.shared.lock();
        s.token = new_token();
        s.sessions.clear();
        (s.allowed, s.token.clone())
    };
    tracing::info!("agent token reset");
    (inner.deps.save)(allowed, &token);
    if allowed {
        write_disc(inner);
    }
}

fn agents_json(inner: &Inner) -> Value {
    let s = inner.shared.lock();
    let registered: Vec<Value> = s
        .registered
        .iter()
        .map(|r| json!({ "app": r.app, "ok": r.result.is_ok(), "error": r.result.as_ref().err() }))
        .collect();
    json!({
        "allowed": s.allowed,
        "url": format!("http://127.0.0.1:{}/mcp", inner.port),
        "registered": registered,
        "activity": inner.activity.lock().to_json(),
        "snippets": snippets(&inner.deps.exe, &format!("http://127.0.0.1:{}/mcp", inner.port)),
    })
}

fn accept_loop(listener: TcpListener, inner: Arc<Inner>) {
    for conn in listener.incoming() {
        if inner.stop.load(Ordering::Acquire) {
            return;
        }
        let Ok(stream) = conn else { continue };
        if inner.active.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            inner.active.fetch_sub(1, Ordering::AcqRel);
            let mut s = stream;
            let _ = respond(&mut s, 503, "text/plain", b"busy", &[]);
            continue;
        }
        let inner2 = inner.clone();
        let spawned = std::thread::Builder::new()
            .name("p2c-tray-agent-conn".into())
            .spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handle(&inner2, stream)
                }));
                if r.is_err() {
                    tracing::error!("an agent request panicked");
                }
                inner2.active.fetch_sub(1, Ordering::AcqRel);
            });
        if spawned.is_err() {
            inner.active.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// Read one request; `Err(status)` for a bad or too large one.
fn read_request(
    r: &mut BufReader<TcpStream>,
    max_body: impl Fn(&str) -> usize,
) -> Result<Request, u16> {
    let mut head_len = 0usize;
    let mut line = String::new();
    r.read_line(&mut line).map_err(|_| 400u16)?;
    head_len += line.len();
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or(400u16)?.to_string();
    let target = parts.next().ok_or(400u16)?;
    let path = target.split('?').next().unwrap_or("").to_string();
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        let n = r.read_line(&mut h).map_err(|_| 400u16)?;
        head_len += n;
        if head_len > MAX_HEAD {
            return Err(431);
        }
        if n == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = match headers.get("content-length") {
        Some(v) => v.parse().map_err(|_| 400u16)?,
        None => 0,
    };
    if headers
        .get("transfer-encoding")
        .is_some_and(|v| v.eq_ignore_ascii_case("chunked"))
    {
        return Err(411);
    }
    if len > max_body(&path) {
        return Err(413);
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).map_err(|_| 400u16)?;
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

fn respond(
    s: &mut TcpStream,
    code: u16,
    kind: &str,
    body: &[u8],
    extra: &[(&str, String)],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        reason(code),
        body.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    s.flush()?;
    let _ = s.shutdown(std::net::Shutdown::Write);
    Ok(())
}

fn json_reply(s: &mut TcpStream, code: u16, v: &Value, extra: &[(&str, String)]) {
    let _ = respond(s, code, "application/json", v.to_string().as_bytes(), extra);
}

fn bearer(req: &Request) -> Option<&str> {
    let v = req.headers.get("authorization")?;
    let (scheme, t) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(t.trim())
}

fn handle(inner: &Arc<Inner>, stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut s = stream;
    let mut r = BufReader::new(read_half);
    let req = match read_request(&mut r, |p| {
        if p == "/mcp" {
            MAX_MCP_BODY
        } else {
            MAX_TRAY_BODY
        }
    }) {
        Ok(x) => x,
        Err(code) => {
            let _ = respond(&mut s, code, "text/plain", reason(code).as_bytes(), &[]);
            return;
        }
    };
    let port = inner.port;
    let host_ok = req
        .headers
        .get("host")
        .is_some_and(|h| *h == format!("127.0.0.1:{port}") || *h == format!("localhost:{port}"));
    // Browsers send Origin: only our own (none for native clients).
    let origin_ok = req.headers.get("origin").is_none_or(|o| {
        *o == format!("http://127.0.0.1:{port}") || *o == format!("http://localhost:{port}")
    });
    if !host_ok || !origin_ok {
        let _ = respond(&mut s, 403, "text/plain", b"forbidden", &[]);
        return;
    }
    match req.path.as_str() {
        "/tray/agents" => tray_route(inner, &req, &mut s),
        "/mcp" => mcp_route(inner, &req, &mut s),
        _ => {
            let _ = respond(&mut s, 404, "text/plain", b"not found", &[]);
        }
    }
    // Let the client read everything before the socket goes.
    let _ = std::io::copy(&mut r.take(64 * 1024), &mut std::io::sink());
}

fn tray_route(inner: &Arc<Inner>, req: &Request, s: &mut TcpStream) {
    let ok = bearer(req)
        .is_some_and(|t| constant_time_eq(t.as_bytes(), inner.deps.tray_token.as_bytes()));
    if !ok {
        json_reply(s, 401, &json!({ "error": "unauthorized" }), &[]);
        return;
    }
    match req.method.as_str() {
        "GET" => json_reply(s, 200, &agents_json(inner), &[]),
        "POST" => {
            let v: Value = match serde_json::from_slice(&req.body) {
                Ok(v) => v,
                Err(_) => {
                    json_reply(s, 400, &json!({ "error": "body must be JSON" }), &[]);
                    return;
                }
            };
            if v["reset_token"] == json!(true) {
                reset_token(inner);
            } else if let Some(on) = v["allowed"].as_bool() {
                set_allowed(inner, on);
            } else {
                json_reply(
                    s,
                    400,
                    &json!({ "error": "give allowed or reset_token" }),
                    &[],
                );
                return;
            }
            json_reply(s, 200, &agents_json(inner), &[]);
        }
        _ => json_reply(
            s,
            405,
            &json!({ "error": "method not allowed" }),
            &[("Allow", "GET, POST".into())],
        ),
    }
}

fn mcp_route(inner: &Arc<Inner>, req: &Request, s: &mut TcpStream) {
    let (allowed, token) = {
        let sh = inner.shared.lock();
        (sh.allowed, sh.token.clone())
    };
    if !allowed {
        let _ = respond(s, 404, "text/plain", b"agents are switched off", &[]);
        return;
    }
    if !bearer(req).is_some_and(|t| constant_time_eq(t.as_bytes(), token.as_bytes())) {
        json_reply(
            s,
            401,
            &json!({ "error": "unauthorized" }),
            &[("WWW-Authenticate", "Bearer".into())],
        );
        return;
    }
    let session = req.headers.get("mcp-session-id").cloned();
    match req.method.as_str() {
        "POST" => {}
        "DELETE" => {
            if let Some(id) = session {
                inner.shared.lock().sessions.retain(|(s, _)| *s != id);
            }
            let _ = respond(s, 200, "text/plain", b"", &[]);
            return;
        }
        _ => {
            let _ = respond(
                s,
                405,
                "text/plain",
                b"POST only",
                &[("Allow", "POST, DELETE".into())],
            );
            return;
        }
    }
    let msg: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(_) => {
            json_reply(
                s,
                400,
                &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } }),
                &[],
            );
            return;
        }
    };
    let is_init = msg["method"] == "initialize";
    let mut client = if is_init {
        ClientInfo::default()
    } else {
        session
            .as_ref()
            .and_then(|id| {
                inner
                    .shared
                    .lock()
                    .sessions
                    .iter()
                    .find(|(s, _)| s == id)
                    .map(|(_, c)| c.clone())
            })
            .unwrap_or_default()
    };
    let answer = inner.mcp.handle(&msg, &mut client);
    let mut extra = Vec::new();
    if is_init {
        let id = format!(
            "{}-{}",
            new_token(),
            inner.session_seq.fetch_add(1, Ordering::Relaxed)
        );
        let mut sh = inner.shared.lock();
        if sh.sessions.len() >= MAX_SESSIONS {
            sh.sessions.pop_front();
        }
        sh.sessions.push_back((id.clone(), client));
        extra.push(("Mcp-Session-Id", id));
    }
    match answer {
        Some(v) => json_reply(s, 200, &v, &extra),
        None => {
            let _ = respond(s, 202, "application/json", b"", &extra);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_backend::tests::FakeTray;

    struct Rig {
        server: AgentServer,
        saved: Arc<Mutex<Vec<(bool, String)>>>,
        regs: Arc<Mutex<Vec<bool>>>,
        disc: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn rig(preferred: u16, allowed: bool) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let disc = dir.path().join("agent.json");
        let saved = Arc::new(Mutex::new(Vec::new()));
        let regs = Arc::new(Mutex::new(Vec::new()));
        let (s2, r2) = (saved.clone(), regs.clone());
        let tray = Arc::new(FakeTray::default());
        *tray.state.lock() = "stopped".into();
        let activity = Arc::new(Mutex::new(Activity::default()));
        let backend = TrayBackend::new(tray, dir.path().join("pages"), activity.clone());
        let deps = ServerDeps {
            tray_token: "traytoken".into(),
            discovery: Some(disc.clone()),
            exe: "C:\\p\\page2cam.exe".into(),
            save: Box::new(move |on, t| s2.lock().push((on, t.to_string()))),
            registrar: Arc::new(move |on| {
                r2.lock().push(on);
                vec![RegResult {
                    app: "Claude Desktop".into(),
                    result: Ok(()),
                }]
            }),
        };
        let server = AgentServer::start(
            preferred,
            allowed,
            Some("a".repeat(32)),
            deps,
            backend,
            activity,
        )
        .unwrap();
        Rig {
            server,
            saved,
            regs,
            disc,
            _dir: dir,
        }
    }

    /// One request; returns (status, headers text, body).
    fn req(
        port: u16,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> (u16, String, String) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut h = format!(
            "{method} {path} HTTP/1.1\r\nContent-Length: {}\r\n",
            body.len()
        );
        for (k, v) in headers {
            h.push_str(&format!("{k}: {v}\r\n"));
        }
        h.push_str("\r\n");
        s.write_all(h.as_bytes()).unwrap();
        s.write_all(body.as_bytes()).unwrap();
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).unwrap();
        let text = String::from_utf8_lossy(&raw).to_string();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        let code = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (code, head.to_string(), body.to_string())
    }

    fn host(port: u16) -> (&'static str, String) {
        ("Host", format!("127.0.0.1:{port}"))
    }

    fn auth(t: &str) -> (&'static str, String) {
        ("Authorization", format!("Bearer {t}"))
    }

    const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","clientInfo":{"name":"Claude Desktop"}}}"#;

    fn wait_until(f: impl Fn() -> bool) {
        for _ in 0..200 {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("condition not reached");
    }

    #[test]
    fn mcp_404_when_agents_off() {
        let r = rig(0, false);
        let p = r.server.port();
        let (code, _, _) = req(p, "POST", "/mcp", &[host(p), auth(&"a".repeat(32))], INIT);
        assert_eq!(code, 404);
        assert!(!r.disc.exists());
    }

    #[test]
    fn mcp_requires_bearer_and_host() {
        let r = rig(0, true);
        let p = r.server.port();
        let tok = "a".repeat(32);
        assert_eq!(req(p, "POST", "/mcp", &[host(p)], INIT).0, 401);
        assert_eq!(
            req(p, "POST", "/mcp", &[host(p), auth("wrong")], INIT).0,
            401
        );
        assert_eq!(
            req(
                p,
                "POST",
                "/mcp",
                &[("Host", "evil.example".into()), auth(&tok)],
                INIT
            )
            .0,
            403
        );
        assert_eq!(
            req(
                p,
                "POST",
                "/mcp",
                &[
                    host(p),
                    auth(&tok),
                    ("Origin", "http://evil.example".into())
                ],
                INIT
            )
            .0,
            403
        );
        let (code, head, body) = req(p, "POST", "/mcp", &[host(p), auth(&tok)], INIT);
        assert_eq!(code, 200, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["serverInfo"]["name"], "page2cam");
        let sid = head
            .lines()
            .find_map(|l| l.strip_prefix("Mcp-Session-Id: "))
            .unwrap()
            .to_string();
        // A notification: 202, no body.
        let (code, _, body) = req(
            p,
            "POST",
            "/mcp",
            &[host(p), auth(&tok), ("Mcp-Session-Id", sid.clone())],
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        );
        assert_eq!((code, body.as_str()), (202, ""));
        // The session remembers the client: its activity carries the name.
        let (code, _, body) = req(
            p,
            "POST",
            "/mcp",
            &[host(p), auth(&tok), ("Mcp-Session-Id", sid)],
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_pages","arguments":{}}}"#,
        );
        assert_eq!(code, 200, "{body}");
        assert_eq!(req(p, "GET", "/mcp", &[host(p), auth(&tok)], "").0, 405);
        assert_eq!(
            req(p, "POST", "/mcp", &[host(p), auth(&tok)], "{nope").0,
            400
        );
        assert_eq!(req(p, "GET", "/other", &[host(p)], "").0, 404);
    }

    #[test]
    fn tray_routes_require_tray_token() {
        let r = rig(0, false);
        let p = r.server.port();
        assert_eq!(req(p, "GET", "/tray/agents", &[host(p)], "").0, 401);
        assert_eq!(
            req(
                p,
                "GET",
                "/tray/agents",
                &[host(p), auth(&"a".repeat(32))],
                ""
            )
            .0,
            401
        );
        assert_eq!(
            req(
                p,
                "GET",
                "/tray/agents",
                &[("Host", "x:1".into()), auth("traytoken")],
                ""
            )
            .0,
            403
        );
        let (code, _, body) = req(p, "GET", "/tray/agents", &[host(p), auth("traytoken")], "");
        assert_eq!(code, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["allowed"], false);
        assert!(v["snippets"]["stdio"].as_str().unwrap().contains("--mcp"));
        // No token anywhere in the panel's data.
        assert!(
            !body.contains(&"a".repeat(32)) && !body.contains("traytoken"),
            "{body}"
        );
    }

    #[test]
    fn switch_on_then_off_closes_mcp_and_cleans_up() {
        let r = rig(0, false);
        let p = r.server.port();
        let (code, _, body) = req(
            p,
            "POST",
            "/tray/agents",
            &[host(p), auth("traytoken")],
            r#"{"allowed":true}"#,
        );
        assert_eq!(code, 200, "{body}");
        assert!(r.disc.exists());
        let (url, tok) =
            crate::agents::parse_discovery(&std::fs::read_to_string(&r.disc).unwrap()).unwrap();
        assert_eq!(url, format!("http://127.0.0.1:{p}/mcp"));
        assert_eq!(req(p, "POST", "/mcp", &[host(p), auth(&tok)], INIT).0, 200);
        wait_until(|| r.regs.lock().as_slice() == [true]);
        assert!(r.saved.lock().last().unwrap().0);
        // Off: 404, file gone, unregistered, saved.
        req(
            p,
            "POST",
            "/tray/agents",
            &[host(p), auth("traytoken")],
            r#"{"allowed":false}"#,
        );
        assert_eq!(req(p, "POST", "/mcp", &[host(p), auth(&tok)], INIT).0, 404);
        assert!(!r.disc.exists());
        wait_until(|| r.regs.lock().as_slice() == [true, false]);
        assert!(!r.saved.lock().last().unwrap().0);
    }

    #[test]
    fn port_falls_back_when_8766_taken() {
        let blocker = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let taken = blocker.local_addr().unwrap().port();
        let r = rig(taken, true);
        assert_ne!(r.server.port(), taken);
        let (url, _) =
            crate::agents::parse_discovery(&std::fs::read_to_string(&r.disc).unwrap()).unwrap();
        assert_eq!(url, format!("http://127.0.0.1:{}/mcp", r.server.port()));
    }

    #[test]
    fn reset_token_rotates_and_rewrites_discovery() {
        let r = rig(0, true);
        let p = r.server.port();
        let old = "a".repeat(32);
        req(
            p,
            "POST",
            "/tray/agents",
            &[host(p), auth("traytoken")],
            r#"{"reset_token":true}"#,
        );
        let (_, new) =
            crate::agents::parse_discovery(&std::fs::read_to_string(&r.disc).unwrap()).unwrap();
        assert_ne!(new, old);
        assert_eq!(req(p, "POST", "/mcp", &[host(p), auth(&old)], INIT).0, 401);
        assert_eq!(req(p, "POST", "/mcp", &[host(p), auth(&new)], INIT).0, 200);
        assert_eq!(r.saved.lock().last().unwrap(), &(true, new));
    }

    #[test]
    fn dropping_the_server_removes_discovery() {
        let r = rig(0, true);
        assert!(r.disc.exists());
        let disc = r.disc.clone();
        drop(r.server);
        assert!(!disc.exists());
    }

    #[test]
    fn oversized_bodies_are_refused() {
        let r = rig(0, true);
        let p = r.server.port();
        let mut s = TcpStream::connect(("127.0.0.1", p)).unwrap();
        let h = format!(
            "POST /tray/agents HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nAuthorization: Bearer traytoken\r\nContent-Length: {}\r\n\r\n",
            MAX_TRAY_BODY + 1
        );
        s.write_all(h.as_bytes()).unwrap();
        let mut raw = String::new();
        s.read_to_string(&mut raw).unwrap();
        assert!(raw.starts_with("HTTP/1.1 413"), "{raw}");
    }
}
