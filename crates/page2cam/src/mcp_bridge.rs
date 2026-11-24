//! `page2cam.exe --mcp`: the stdio MCP server desktop agents start. It is a
//! thin bridge: newline-delimited JSON-RPC from stdin goes to the tray's
//! local `/mcp` endpoint (address and token from `agent.json`), answers go to
//! stdout. Nothing but JSON-RPC is ever written to stdout; logs go to
//! `page2cam-mcp.log`.

use std::io::{BufRead, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// How long one forwarded message may take (start waits up to 40 s).
const CALL_TIMEOUT: Duration = Duration::from_secs(90);
/// How long a launched tray gets to write `agent.json`.
const TRAY_WAIT: Duration = Duration::from_secs(10);
const MAX_ANSWER: usize = 64 * 1024 * 1024;

const NOT_RUNNING: &str =
    "Page2Cam is not running: start it from the Start menu (it waits in the tray)";

/// `CreateProcess` flags for the tray we start: not in the agent's console,
/// job or Ctrl-C group, so closing the agent never closes Page2Cam.
pub fn detached_launch_flags() -> u32 {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB
}

/// The bridge's surroundings (real: tray.toml, agent.json, HTTP; tests: fakes).
pub trait Env {
    fn agents_allowed(&self) -> bool;
    /// `(url, token)` from agent.json.
    fn discovery(&self) -> Option<(String, String)>;
    fn tray_running(&self) -> bool;
    /// Start the tray detached; false if it could not be started.
    fn launch_tray(&self) -> bool;
    /// POST one message (with the agent's name when known); returns
    /// (status, session id header, body).
    fn post(
        &self,
        url: &str,
        token: &str,
        session: Option<&str>,
        agent: Option<&str>,
        body: &[u8],
    ) -> PostResult;
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d)
    }
}

/// A failed POST. `sent` = the request may have reached the tray (it may
/// have acted on it), so it must not be sent again.
#[derive(Debug)]
pub struct PostError {
    pub sent: bool,
    pub msg: String,
}

impl PostError {
    fn unsent(msg: impl Into<String>) -> Self {
        PostError {
            sent: false,
            msg: msg.into(),
        }
    }
    fn sent(msg: impl Into<String>) -> Self {
        PostError {
            sent: true,
            msg: msg.into(),
        }
    }
}

pub type PostResult = Result<(u16, Option<String>, Vec<u8>), PostError>;

pub struct Bridge<E: Env> {
    env: E,
    /// The client's `initialize`, replayed when the tray restarted.
    init: Option<Value>,
    /// The client's name from `initialize` (sent along, for the activity list).
    agent: Option<String>,
    session: Option<String>,
    endpoint: Option<(String, String)>,
}

fn local_error(id: Value, code: i64, msg: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg } })
}

fn tool_error(id: Value, msg: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": {
        "content": [{ "type": "text", "text": msg }], "isError": true } })
}

impl<E: Env> Bridge<E> {
    pub fn new(env: E) -> Self {
        Bridge {
            env,
            init: None,
            agent: None,
            session: None,
            endpoint: None,
        }
    }

    /// Answers when Page2Cam can't be reached: `initialize`, `ping` and
    /// `tools/list` still work (so the agent sees the tools and can tell the
    /// user what to do); every tool call is an error with `why`.
    fn local(&self, msg: &Value, why: &str) -> Option<Value> {
        let id = msg.get("id")?.clone();
        // The guide, its resources and prompts need no tray.
        if let Some(r) = p2c_mcp::offline(msg) {
            return Some(r);
        }
        let method = msg["method"].as_str().unwrap_or("");
        Some(match method {
            "initialize" => {
                let asked = msg["params"]["protocolVersion"].as_str().unwrap_or("");
                let v = if p2c_mcp::SUPPORTED_VERSIONS.contains(&asked) {
                    asked
                } else {
                    p2c_mcp::PROTOCOL_VERSION
                };
                json!({ "jsonrpc": "2.0", "id": id, "result": {
                    "protocolVersion": v,
                    "capabilities": { "tools": { "listChanged": false },
                        "resources": { "listChanged": false }, "prompts": { "listChanged": false } },
                    "serverInfo": { "name": "page2cam", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": format!("Page2Cam can't be controlled right now: {why}."),
                } })
            }
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            "tools/list" => {
                json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": p2c_mcp::tool_defs() } })
            }
            "tools/call" => tool_error(id, why),
            "" => local_error(id, -32600, "invalid request"),
            m => local_error(id, -32601, &format!("method not found: {m}")),
        })
    }

    /// Start the tray (detached) and wait for its agent.json.
    fn start_tray(&mut self) -> bool {
        tracing::info!("no tray running: starting it");
        if !self.env.launch_tray() {
            return false;
        }
        let until = Instant::now() + TRAY_WAIT;
        while Instant::now() < until {
            if self.env.tray_running() && self.env.discovery().is_some() {
                return true;
            }
            self.env.sleep(Duration::from_millis(200));
        }
        false
    }

    /// The tray's address, starting the tray if needed.
    fn endpoint(&mut self) -> Option<(String, String)> {
        if let Some(e) = &self.endpoint {
            return Some(e.clone());
        }
        if self.env.discovery().is_none() && !self.env.tray_running() {
            self.start_tray();
        }
        let found = self.env.discovery();
        self.endpoint = found.clone();
        found
    }

    fn forward(&mut self, msg: &Value) -> Result<Option<Value>, PostError> {
        let (url, token) = self
            .endpoint()
            .ok_or_else(|| PostError::unsent(NOT_RUNNING))?;
        let body = msg.to_string();
        let (code, session, out) = self.env.post(
            &url,
            &token,
            self.session.as_deref(),
            self.agent.as_deref(),
            body.as_bytes(),
        )?;
        match code {
            200 | 202 => {
                if let Some(s) = session {
                    self.session = Some(s);
                }
                if out.is_empty() || code == 202 {
                    return Ok(None);
                }
                serde_json::from_slice(&out)
                    .map(Some)
                    .map_err(|_| PostError::sent("the tray sent a bad answer"))
            }
            // Refused before acting (agents off, or an old token/port).
            401 | 404 => Err(PostError::unsent(p2c_mcp::AGENTS_OFF)),
            c => Err(PostError::sent(format!("the tray answered {c}"))),
        }
    }

    /// Send the handshake again (after the tray restarted), then `msg`.
    fn reconnect_and_forward(&mut self, msg: &Value) -> Result<Option<Value>, PostError> {
        self.endpoint = None;
        self.session = None;
        if let Some(init) = self.init.clone().filter(|_| msg["method"] != "initialize") {
            self.forward(&init)?;
            let _ =
                self.forward(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        }
        self.forward(msg)
    }

    /// The answer to one message from the agent (`None`: nothing to send).
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        if msg["method"] == "initialize" {
            self.init = Some(msg.clone());
            self.session = None;
            if let Some(n) = msg["params"]["clientInfo"]["name"].as_str() {
                let n: String = n.chars().filter(|c| !c.is_control()).take(64).collect();
                self.agent = (!n.trim().is_empty()).then_some(n);
            }
        }
        if !self.env.agents_allowed() {
            self.endpoint = None;
            return self.local(msg, p2c_mcp::AGENTS_OFF);
        }
        let first = match self.forward(msg) {
            Ok(a) => return a,
            Err(e) => e,
        };
        // A tool call that may have reached the tray is never sent twice
        // (typing or starting a stream twice); the agent sees the error.
        if first.sent && msg["method"] == "tools/call" {
            tracing::warn!("tool call failed after sending: {}", first.msg);
            self.endpoint = None;
            return self.local(msg, &first.msg);
        }
        tracing::info!("forwarding failed ({}); trying again", first.msg);
        // No tray behind the (maybe stale) agent.json: start one and keep
        // trying while it comes up.
        let launched = !self.env.tray_running() && self.start_tray();
        let until = Instant::now() + if launched { TRAY_WAIT } else { Duration::ZERO };
        loop {
            match self.reconnect_and_forward(msg) {
                Ok(a) => return a,
                Err(e) if !e.sent && Instant::now() < until => {
                    self.env.sleep(Duration::from_millis(200));
                }
                Err(e) => {
                    tracing::warn!("cannot reach the tray: {}", e.msg);
                    self.endpoint = None;
                    return self.local(msg, &e.msg);
                }
            }
        }
    }

    /// Serve until stdin closes. Every output line is one JSON-RPC message.
    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) {
        for line in input.lines() {
            let Ok(line) = line else { break };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let answer = match serde_json::from_str::<Value>(line) {
                Ok(msg) if msg.is_object() => self.handle(&msg),
                Ok(_) => Some(local_error(Value::Null, -32600, "invalid request")),
                Err(_) => Some(local_error(Value::Null, -32700, "parse error")),
            };
            if let Some(a) = answer {
                let text = a.to_string();
                if writeln!(output, "{text}")
                    .and_then(|()| output.flush())
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

/// A failed read of the tray's answer. A reset before any answer byte is
/// taken as the tray refusing early (401/404 before reading the body), so
/// the call may go again. Caveat: the tray acts first and answers after,
/// so if it dies mid-call (reset, no answer yet) a call it already carried
/// out can be sent again. Any other failure may have been acted on and is
/// not repeated.
fn read_failure(e: std::io::Error, got_answer: bool) -> PostError {
    use std::io::ErrorKind;
    let reset = matches!(
        e.kind(),
        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
    );
    if reset && !got_answer {
        PostError::unsent(e.to_string())
    } else {
        PostError::sent(e.to_string())
    }
}

/// One HTTP POST to the tray (`url` = `http://127.0.0.1:<port>/mcp`).
pub fn http_post(
    url: &str,
    token: &str,
    session: Option<&str>,
    agent: Option<&str>,
    body: &[u8],
) -> PostResult {
    let unsent = |e: std::io::Error| PostError::unsent(e.to_string());
    let sent = |e: std::io::Error| PostError::sent(e.to_string());
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| PostError::unsent("bad tray address"))?;
    let (host, path) = rest.split_once('/').map_or((rest, "/"), |(h, p)| (h, p));
    let path = format!("/{}", path.trim_start_matches('/'));
    let addr: SocketAddr = host
        .to_socket_addrs()
        .map_err(unsent)?
        .next()
        .ok_or_else(|| PostError::unsent("bad tray address"))?;
    if !addr.ip().is_loopback() {
        return Err(PostError::unsent("the tray address is not local"));
    }
    let deadline = Instant::now() + CALL_TIMEOUT;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(unsent)?;
    s.set_write_timeout(Some(CALL_TIMEOUT)).map_err(unsent)?;
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(sid) = session {
        head.push_str(&format!("Mcp-Session-Id: {sid}\r\n"));
    }
    if let Some(a) = agent.filter(|a| a.chars().all(|c| !c.is_control())) {
        head.push_str(&format!("X-P2C-Agent: {a}\r\n"));
    }
    head.push_str("\r\n");
    // The tray acts only on a complete body: a failed write was not acted on.
    s.write_all(head.as_bytes()).map_err(unsent)?;
    s.write_all(body).map_err(unsent)?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| PostError::sent("the tray did not answer in time"))?;
        s.set_read_timeout(Some(left)).map_err(sent)?;
        let n = s
            .read(&mut buf)
            .map_err(|e| read_failure(e, !raw.is_empty()))?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if raw.len() > MAX_ANSWER {
            return Err(PostError::sent("the tray's answer is too large"));
        }
    }
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| PostError::sent("incomplete answer"))?;
    let head = String::from_utf8_lossy(&raw[..end]).to_string();
    let session = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("mcp-session-id")
            .then(|| v.trim().to_string())
    });
    let (code, body) =
        p2c_tray::http::parse_response_bytes(&raw).map_err(|e| PostError::sent(e.to_string()))?;
    Ok((code, session, body))
}

/// Bridge logs kept (the newest; the current one is among them).
const KEEP_LOGS: usize = 5;

/// Delete all but the `keep` newest `page2cam-mcp-*.log` (and their
/// `.prev.log`) in `dir`. A log another bridge still writes can't be
/// deleted on Windows and is simply left.
fn prune_bridge_logs(dir: &std::path::Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<(std::time::SystemTime, std::path::PathBuf)> = rd
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("page2cam-mcp-") && n.ends_with(".log")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    logs.sort_by_key(|l| std::cmp::Reverse(l.0));
    for (_, p) in logs.into_iter().skip(keep) {
        let _ = std::fs::remove_file(p);
    }
}

/// The real surroundings.
struct RealEnv;

impl Env for RealEnv {
    fn agents_allowed(&self) -> bool {
        p2c_tray::settings::agents_allowed_in(&p2c_tray::settings::read_tray_toml())
    }

    fn discovery(&self) -> Option<(String, String)> {
        let p = p2c_tray::agents::discovery_path()?;
        p2c_tray::agents::parse_discovery(&std::fs::read_to_string(p).ok()?)
    }

    fn tray_running(&self) -> bool {
        p2c_tray::tray_running()
    }

    #[cfg(windows)]
    fn launch_tray(&self) -> bool {
        use std::os::windows::process::CommandExt;
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let spawn = |flags: u32| {
            std::process::Command::new(&exe)
                .creation_flags(flags)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        };
        // Breaking away from the agent's job can be refused: then without.
        match spawn(detached_launch_flags())
            .or_else(|_| spawn(detached_launch_flags() & !0x0100_0000))
        {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!("cannot start the tray: {e}");
                false
            }
        }
    }

    #[cfg(not(windows))]
    fn launch_tray(&self) -> bool {
        false
    }

    fn post(
        &self,
        url: &str,
        token: &str,
        session: Option<&str>,
        agent: Option<&str>,
        body: &[u8],
    ) -> PostResult {
        http_post(url, token, session, agent, body)
    }
}

/// Run the bridge on stdin/stdout; returns the exit code.
pub fn run() -> i32 {
    // One log per bridge (several agents run their own at once).
    let log =
        crate::logging::init_named(false, &format!("page2cam-mcp-{}.log", std::process::id()));
    if let Some(dir) = log.as_deref().and_then(std::path::Path::parent) {
        prune_bridge_logs(dir, KEEP_LOGS);
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "MCP bridge starting");
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    if !cfg!(windows) {
        // No tray on Linux yet: answer every call with that.
        struct Off;
        impl Env for Off {
            fn agents_allowed(&self) -> bool {
                true
            }
            fn discovery(&self) -> Option<(String, String)> {
                None
            }
            fn tray_running(&self) -> bool {
                true
            }
            fn launch_tray(&self) -> bool {
                false
            }
            fn post(
                &self,
                _: &str,
                _: &str,
                _: Option<&str>,
                _: Option<&str>,
                _: &[u8],
            ) -> PostResult {
                Err(PostError::unsent("agent control is Windows-only for now"))
            }
        }
        let mut b = Bridge::new(Off);
        b.serve(stdin.lock(), stdout.lock());
        return 0;
    }
    let mut b = Bridge::new(RealEnv);
    b.serve(stdin.lock(), stdout.lock());
    tracing::info!("MCP bridge: input closed");
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct Fake {
        allowed: Cell<bool>,
        disc: RefCell<Option<(String, String)>>,
        running: Cell<bool>,
        launched: Cell<u32>,
        /// Discovery that appears once the tray is launched.
        after_launch: RefCell<Option<(String, String)>>,
        posts: RefCell<Vec<(String, Option<String>, Value)>>,
        /// Every post fails (connect refused, never sent).
        fail: Cell<bool>,
        /// Posts fail while the tray isn't running (a stale agent.json).
        need_tray: Cell<bool>,
        /// The next post fails after sending (a timeout): `Some(count)` left.
        fail_after_send: Cell<u32>,
        /// Agent names the posts carried.
        agents: RefCell<Vec<Option<String>>>,
    }

    impl Env for &Fake {
        fn agents_allowed(&self) -> bool {
            self.allowed.get()
        }
        fn discovery(&self) -> Option<(String, String)> {
            self.disc.borrow().clone()
        }
        fn tray_running(&self) -> bool {
            self.running.get()
        }
        fn launch_tray(&self) -> bool {
            self.launched.set(self.launched.get() + 1);
            self.running.set(true);
            if let Some(d) = self.after_launch.borrow().clone() {
                *self.disc.borrow_mut() = Some(d);
            }
            true
        }
        fn post(
            &self,
            url: &str,
            token: &str,
            session: Option<&str>,
            agent: Option<&str>,
            body: &[u8],
        ) -> PostResult {
            if self.fail.get() || (self.need_tray.get() && !self.running.get()) {
                return Err(PostError::unsent("connection refused"));
            }
            assert_eq!(token, "tok");
            let msg: Value = serde_json::from_slice(body).unwrap();
            self.posts
                .borrow_mut()
                .push((url.into(), session.map(String::from), msg.clone()));
            self.agents.borrow_mut().push(agent.map(String::from));
            if self.fail_after_send.get() > 0 {
                self.fail_after_send.set(self.fail_after_send.get() - 1);
                return Err(PostError::sent("the tray did not answer in time"));
            }
            let Some(id) = msg.get("id") else {
                return Ok((202, None, Vec::new()));
            };
            let sid = (msg["method"] == "initialize").then(|| "S1".to_string());
            let answer = json!({ "jsonrpc": "2.0", "id": id, "result": { "echo": msg["method"] } });
            Ok((200, sid, answer.to_string().into_bytes()))
        }
        fn sleep(&self, _: Duration) {}
    }

    fn run_lines(b: &mut Bridge<&Fake>, input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        b.serve(input.as_bytes(), &mut out);
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).expect("every output line is JSON"))
            .collect()
    }

    const INIT: &str = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","clientInfo":{"name":"Codex"}}}"#;

    const CALL: &str = r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"type_text","arguments":{"text":"hi"}}}"#;

    fn disc() -> Option<(String, String)> {
        Some(("http://127.0.0.1:8766/mcp".into(), "tok".into()))
    }

    #[test]
    fn bridge_starts_the_tray_behind_a_stale_discovery_file() {
        // agent.json left behind by a crash; no tray runs.
        let f = Fake::default();
        f.allowed.set(true);
        f.need_tray.set(true);
        *f.disc.borrow_mut() = disc();
        let mut b = Bridge::new(&f);
        let out = run_lines(&mut b, &format!("{INIT}\n{CALL}\n"));
        assert_eq!(f.launched.get(), 1);
        assert_eq!(out[0]["result"]["echo"], "initialize");
        assert_eq!(out[1]["result"]["echo"], "tools/call");
    }

    #[test]
    fn bridge_never_repeats_a_tool_call_that_was_sent() {
        let f = Fake::default();
        f.allowed.set(true);
        f.running.set(true);
        *f.disc.borrow_mut() = disc();
        let mut b = Bridge::new(&f);
        run_lines(&mut b, &format!("{INIT}\n"));
        f.fail_after_send.set(1);
        let out = run_lines(&mut b, CALL);
        assert_eq!(out[0]["result"]["isError"], true, "{out:?}");
        let calls = f
            .posts
            .borrow()
            .iter()
            .filter(|p| p.2["method"] == "tools/call")
            .count();
        assert_eq!(calls, 1, "typed once, not twice");
        // Other methods may be repeated.
        f.fail_after_send.set(1);
        let out = run_lines(&mut b, r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#);
        assert_eq!(out[0]["result"]["echo"], "tools/list");
    }

    #[test]
    fn bridge_passes_the_agent_name_along() {
        let f = Fake::default();
        // Off at first: initialize is answered by the bridge itself...
        f.allowed.set(false);
        f.running.set(true);
        *f.disc.borrow_mut() = disc();
        let mut b = Bridge::new(&f);
        run_lines(&mut b, &format!("{INIT}\n"));
        assert!(f.posts.borrow().is_empty());
        // ...then switched on: the tray still learns who calls.
        f.allowed.set(true);
        run_lines(&mut b, CALL);
        assert_eq!(f.agents.borrow().last().unwrap().as_deref(), Some("Codex"));
    }

    #[test]
    fn only_the_newest_bridge_logs_are_kept() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..8 {
            std::fs::write(d.path().join(format!("page2cam-mcp-{i}.log")), "x").unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::write(d.path().join("page2cam.log"), "engine").unwrap();
        prune_bridge_logs(d.path(), 5);
        let mut left: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "page2cam-mcp-3.log",
                "page2cam-mcp-4.log",
                "page2cam-mcp-5.log",
                "page2cam-mcp-6.log",
                "page2cam-mcp-7.log",
                "page2cam.log"
            ]
        );
    }

    #[test]
    fn bridge_forwards_and_preserves_ids() {
        let f = Fake::default();
        f.allowed.set(true);
        *f.disc.borrow_mut() = Some(("http://127.0.0.1:8766/mcp".into(), "tok".into()));
        let mut b = Bridge::new(&f);
        let input = format!(
            "{INIT}\n{}\n{}\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":"x-7","method":"tools/list"}"#
        );
        let out = run_lines(&mut b, &input);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0]["id"], 0);
        assert_eq!(out[1]["id"], "x-7");
        assert_eq!(out[1]["result"]["echo"], "tools/list");
        let posts = f.posts.borrow();
        assert_eq!(posts.len(), 3);
        assert_eq!(posts[0].1, None);
        // The session from initialize goes with later messages.
        assert_eq!(posts[2].1.as_deref(), Some("S1"));
        assert_eq!(posts[2].0, "http://127.0.0.1:8766/mcp");
    }

    #[test]
    fn bridge_agents_off_answers_locally() {
        let f = Fake::default();
        f.allowed.set(false);
        let mut b = Bridge::new(&f);
        let input = format!(
            "{INIT}\n{}\n{}\n{}\n",
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"status","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"guide","arguments":{"topic":"webinar"}}}"#
        );
        let out = run_lines(&mut b, &input);
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18");
        assert!(out[0]["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("Allow agents"));
        assert_eq!(out[1]["result"]["tools"].as_array().unwrap().len(), 46);
        assert_eq!(out[2]["result"]["isError"], true);
        assert_eq!(out[2]["result"]["content"][0]["text"], p2c_mcp::AGENTS_OFF);
        // The guide works without the tray.
        assert_eq!(out[3]["result"]["isError"], false, "{}", out[3]);
        assert_eq!(f.launched.get(), 0, "no tray launch while agents are off");
        assert!(f.posts.borrow().is_empty());
    }

    #[test]
    fn bridge_starts_the_tray_when_none_runs() {
        let f = Fake::default();
        f.allowed.set(true);
        *f.after_launch.borrow_mut() = Some(("http://127.0.0.1:8766/mcp".into(), "tok".into()));
        let mut b = Bridge::new(&f);
        let out = run_lines(&mut b, &format!("{INIT}\n"));
        assert_eq!(f.launched.get(), 1);
        assert_eq!(out[0]["result"]["echo"], "initialize");
    }

    #[test]
    fn bridge_reports_a_tray_that_is_gone() {
        let f = Fake::default();
        f.allowed.set(true);
        f.running.set(true);
        *f.disc.borrow_mut() = Some(("http://127.0.0.1:8766/mcp".into(), "tok".into()));
        let mut b = Bridge::new(&f);
        run_lines(&mut b, &format!("{INIT}\n"));
        f.fail.set(true);
        let out = run_lines(
            &mut b,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"status"}}"#,
        );
        assert_eq!(out[0]["id"], 5);
        assert_eq!(out[0]["result"]["isError"], true);
        assert!(out[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("connection refused"));
    }

    #[test]
    fn bridge_never_writes_non_jsonrpc_to_stdout() {
        let f = Fake::default();
        f.allowed.set(false);
        let mut b = Bridge::new(&f);
        let mut out = Vec::new();
        b.serve("hello\n\n{bad\n[1,2]\n42\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n".as_bytes(), &mut out);
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        for l in lines {
            let v: Value = serde_json::from_str(l).unwrap();
            assert_eq!(v["jsonrpc"], "2.0");
            assert!(v["error"]["code"].as_i64().is_some(), "{l}");
        }
    }

    #[test]
    fn detached_launch_flags_are_the_three() {
        assert_eq!(
            detached_launch_flags(),
            0x0000_0008 | 0x0000_0200 | 0x0100_0000
        );
    }

    #[test]
    fn http_post_sends_bearer_session_and_host() {
        use std::io::BufReader;
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            let mut body = vec![0u8; 2];
            r.read_exact(&mut body).unwrap();
            s.write_all(b"HTTP/1.1 200 OK\r\nMcp-Session-Id: abc\r\nContent-Length: 2\r\n\r\n{}")
                .unwrap();
            let _ = s.shutdown(std::net::Shutdown::Write);
            let _ = std::io::copy(&mut r, &mut std::io::sink());
            head
        });
        let (code, sid, body) = http_post(
            &format!("http://127.0.0.1:{port}/mcp"),
            "tok",
            Some("S0"),
            Some("Claude Desktop"),
            b"{}",
        )
        .unwrap();
        assert_eq!(
            (code, sid.as_deref(), body.as_slice()),
            (200, Some("abc"), &b"{}"[..])
        );
        let head = t.join().unwrap();
        assert!(head.starts_with("POST /mcp HTTP/1.1\r\n"), "{head}");
        assert!(head.contains(&format!("Host: 127.0.0.1:{port}\r\n")));
        assert!(head.contains("Authorization: Bearer tok\r\n"));
        assert!(head.contains("Mcp-Session-Id: S0\r\n"));
        assert!(head.contains("X-P2C-Agent: Claude Desktop\r\n"));
        assert!(http_post("http://10.0.0.1:1/mcp", "t", None, None, b"{}").is_err());
    }

    #[test]
    fn a_reset_before_any_answer_counts_as_not_sent() {
        use std::io::{Error, ErrorKind};
        // The tray refused early and the close turned into a reset: it did
        // not act, so the call may go again (or the tray be started).
        assert!(!read_failure(Error::from(ErrorKind::ConnectionReset), false).sent);
        assert!(!read_failure(Error::from(ErrorKind::ConnectionAborted), false).sent);
        // Part of an answer came, or it just took too long: it may have acted.
        assert!(read_failure(Error::from(ErrorKind::ConnectionReset), true).sent);
        assert!(read_failure(Error::from(ErrorKind::TimedOut), false).sent);
    }

    #[test]
    fn a_refused_connection_was_never_sent() {
        let port = p2c_tray::launch::pick_free_port().unwrap();
        let e = http_post(
            &format!("http://127.0.0.1:{port}/mcp"),
            "t",
            None,
            None,
            b"{}",
        )
        .unwrap_err();
        assert!(!e.sent, "{e:?}");
    }
}
