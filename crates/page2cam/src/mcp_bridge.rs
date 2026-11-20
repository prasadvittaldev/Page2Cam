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
    /// POST one message; returns (status, session id header, body).
    fn post(
        &self,
        url: &str,
        token: &str,
        session: Option<&str>,
        body: &[u8],
    ) -> Result<(u16, Option<String>, Vec<u8>), String>;
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d)
    }
}

pub struct Bridge<E: Env> {
    env: E,
    /// The client's `initialize`, replayed when the tray restarted.
    init: Option<Value>,
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
            session: None,
            endpoint: None,
        }
    }

    /// Answers when Page2Cam can't be reached: `initialize`, `ping` and
    /// `tools/list` still work (so the agent sees the tools and can tell the
    /// user what to do); every tool call is an error with `why`.
    fn local(&self, msg: &Value, why: &str) -> Option<Value> {
        let id = msg.get("id")?.clone();
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
                    "capabilities": { "tools": { "listChanged": false } },
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

    /// The tray's address, starting the tray if needed.
    fn endpoint(&mut self) -> Option<(String, String)> {
        if let Some(e) = &self.endpoint {
            return Some(e.clone());
        }
        let mut found = self.env.discovery();
        if found.is_none() && !self.env.tray_running() {
            tracing::info!("no tray running: starting it");
            if self.env.launch_tray() {
                let until = Instant::now() + TRAY_WAIT;
                while found.is_none() && Instant::now() < until {
                    self.env.sleep(Duration::from_millis(200));
                    found = self.env.discovery();
                }
            }
        }
        self.endpoint = found.clone();
        found
    }

    fn forward(&mut self, msg: &Value) -> Result<Option<Value>, String> {
        let (url, token) = self.endpoint().ok_or_else(|| NOT_RUNNING.to_string())?;
        let body = msg.to_string();
        let (code, session, out) =
            self.env
                .post(&url, &token, self.session.as_deref(), body.as_bytes())?;
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
                    .map_err(|_| "the tray sent a bad answer".to_string())
            }
            401 | 404 => Err(p2c_mcp::AGENTS_OFF.to_string()),
            c => Err(format!("the tray answered {c}")),
        }
    }

    /// The answer to one message from the agent (`None`: nothing to send).
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        if msg["method"] == "initialize" {
            self.init = Some(msg.clone());
            self.session = None;
        }
        if !self.env.agents_allowed() {
            self.endpoint = None;
            return self.local(msg, p2c_mcp::AGENTS_OFF);
        }
        match self.forward(msg) {
            Ok(a) => a,
            Err(first) => {
                // The tray may have restarted (new port/token/session): look
                // again once, replaying the handshake.
                tracing::info!("forwarding failed ({first}); retrying once");
                self.endpoint = None;
                self.session = None;
                if let Some(init) = self.init.clone().filter(|_| msg["method"] != "initialize") {
                    let _ = self.forward(&init);
                    let _ = self.forward(
                        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                    );
                }
                match self.forward(msg) {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::warn!("cannot reach the tray: {e}");
                        self.endpoint = None;
                        self.local(msg, &e)
                    }
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

/// One HTTP POST to the tray (`url` = `http://127.0.0.1:<port>/mcp`).
pub fn http_post(
    url: &str,
    token: &str,
    session: Option<&str>,
    body: &[u8],
) -> Result<(u16, Option<String>, Vec<u8>), String> {
    let rest = url.strip_prefix("http://").ok_or("bad tray address")?;
    let (host, path) = rest.split_once('/').map_or((rest, "/"), |(h, p)| (h, p));
    let path = format!("/{}", path.trim_start_matches('/'));
    let addr: SocketAddr = host
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("bad tray address")?;
    if !addr.ip().is_loopback() {
        return Err("the tray address is not local".into());
    }
    let deadline = Instant::now() + CALL_TIMEOUT;
    let mut s =
        TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(CALL_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(sid) = session {
        head.push_str(&format!("Mcp-Session-Id: {sid}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    s.write_all(body).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or("the tray did not answer in time")?;
        s.set_read_timeout(Some(left)).map_err(|e| e.to_string())?;
        let n = s.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if raw.len() > MAX_ANSWER {
            return Err("the tray's answer is too large".into());
        }
    }
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("incomplete answer")?;
    let head = String::from_utf8_lossy(&raw[..end]).to_string();
    let session = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("mcp-session-id")
            .then(|| v.trim().to_string())
    });
    let (code, body) = p2c_tray::http::parse_response_bytes(&raw).map_err(|e| e.to_string())?;
    Ok((code, session, body))
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
        body: &[u8],
    ) -> Result<(u16, Option<String>, Vec<u8>), String> {
        http_post(url, token, session, body)
    }
}

/// Run the bridge on stdin/stdout; returns the exit code.
pub fn run() -> i32 {
    crate::logging::init_named(false, "page2cam-mcp.log");
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
                _: &[u8],
            ) -> Result<(u16, Option<String>, Vec<u8>), String> {
                Err("agent control is Windows-only for now".into())
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
        fail: Cell<bool>,
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
            *self.disc.borrow_mut() = self.after_launch.borrow().clone();
            true
        }
        fn post(
            &self,
            url: &str,
            token: &str,
            session: Option<&str>,
            body: &[u8],
        ) -> Result<(u16, Option<String>, Vec<u8>), String> {
            if self.fail.get() {
                return Err("connection refused".into());
            }
            assert_eq!(token, "tok");
            let msg: Value = serde_json::from_slice(body).unwrap();
            self.posts
                .borrow_mut()
                .push((url.into(), session.map(String::from), msg.clone()));
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
            "{INIT}\n{}\n{}\n",
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"status","arguments":{}}}"#
        );
        let out = run_lines(&mut b, &input);
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18");
        assert!(out[0]["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("Allow agents"));
        assert_eq!(out[1]["result"]["tools"].as_array().unwrap().len(), 28);
        assert_eq!(out[2]["result"]["isError"], true);
        assert_eq!(out[2]["result"]["content"][0]["text"], p2c_mcp::AGENTS_OFF);
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
        assert!(http_post("http://10.0.0.1:1/mcp", "t", None, b"{}").is_err());
    }
}
