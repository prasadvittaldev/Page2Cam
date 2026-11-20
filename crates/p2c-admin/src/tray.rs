//! Link to the tray's local control endpoint (`P2C_TRAY_URL` / `P2C_TRAY_TOKEN`).
//!
//! A tiny blocking HTTP/1.1 client over `TcpStream`; callers run it on a
//! blocking thread. The token is never logged.
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(3);
const CACHE_FOR: Duration = Duration::from_secs(5);
const MAX_RESPONSE: usize = 4 * 1024 * 1024;

struct Cache {
    allowed: Option<bool>,
    fetched: Option<Instant>,
    refreshing: bool,
}

pub struct TrayLink {
    host_port: String,
    token: String,
    cache: Mutex<Cache>,
    cache_for: Duration,
    /// Total time for one request (connect, send and read together).
    timeout: Duration,
}

/// Read at most `cap` bytes; more than that is an error, not a truncated body.
fn read_limited<R: Read>(r: R, cap: usize) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    r.take(cap as u64 + 1).read_to_end(&mut out)?;
    if out.len() > cap {
        return Err(bad("response too large"));
    }
    Ok(out)
}

/// A socket whose every read is limited by one overall deadline.
struct DeadlineReader {
    s: TcpStream,
    deadline: Instant,
}

impl Read for DeadlineReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "tray timed out"))?;
        self.s.set_read_timeout(Some(left))?;
        self.s.read(buf)
    }
}

impl TrayLink {
    /// `url` is `http://host:port`; anything else is refused.
    pub fn new(url: &str, token: &str) -> Option<Arc<TrayLink>> {
        Self::with_limits(url, token, CACHE_FOR, TIMEOUT)
    }

    fn with_limits(
        url: &str,
        token: &str,
        cache_for: Duration,
        timeout: Duration,
    ) -> Option<Arc<TrayLink>> {
        let rest = url.trim().strip_prefix("http://")?;
        let host_port = rest.trim_end_matches('/');
        if host_port.is_empty() || host_port.contains('/') || token.is_empty() {
            return None;
        }
        Some(Arc::new(TrayLink {
            host_port: host_port.to_string(),
            token: token.to_string(),
            cache: Mutex::new(Cache {
                allowed: None,
                fetched: None,
                refreshing: false,
            }),
            cache_for,
            timeout,
        }))
    }

    /// The link from the environment, read once (the variables are then
    /// removed, so child processes don't inherit the token); `None` when
    /// standalone. The engine calls it at start, before any child starts.
    pub fn global() -> Option<Arc<TrayLink>> {
        static LINK: OnceLock<Option<Arc<TrayLink>>> = OnceLock::new();
        LINK.get_or_init(|| {
            let (url, token) = take_env()?;
            TrayLink::new(&url, &token)
        })
        .clone()
    }

    /// One request to `/tray/agents`. Returns the tray's status and body.
    pub fn request(&self, method: &str, body: Option<&[u8]>) -> std::io::Result<(u16, Vec<u8>)> {
        let deadline = Instant::now() + self.timeout;
        let left = || {
            deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "tray timed out"))
        };
        let addr = self
            .host_port
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| std::io::Error::other("tray address does not resolve"))?;
        let mut s = TcpStream::connect_timeout(&addr, left()?)?;
        s.set_write_timeout(Some(left()?))?;
        let body = body.unwrap_or(&[]);
        let mut req = format!(
            "{method} /tray/agents HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nAccept: application/json\r\nConnection: close\r\n",
            self.host_port, self.token
        );
        if method == "POST" {
            req.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                body.len()
            ));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes())?;
        s.write_all(body)?;
        // Read with the time left for the whole request, not per read.
        let raw = read_limited(DeadlineReader { s, deadline }, MAX_RESPONSE)?;
        parse_response(&raw)
    }

    /// Last known "agents allowed" (None: unknown or the last refresh failed),
    /// never blocking: a stale value triggers a
    /// background refresh and the old value is returned meanwhile.
    pub fn allowed(self: &Arc<Self>) -> Option<bool> {
        let (value, refresh) = {
            let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            let stale = c.fetched.is_none_or(|t| t.elapsed() > self.cache_for);
            let refresh = stale && !c.refreshing;
            if refresh {
                c.refreshing = true;
            }
            (c.allowed, refresh)
        };
        if refresh {
            let me = self.clone();
            let spawned = std::thread::Builder::new()
                .name("p2c-tray-poll".into())
                .spawn(move || {
                    let got = me
                        .request("GET", None)
                        .ok()
                        .filter(|(code, _)| *code == 200)
                        .and_then(|(_, b)| serde_json::from_slice::<serde_json::Value>(&b).ok())
                        .and_then(|v| v.get("allowed").and_then(|a| a.as_bool()));
                    let mut c = me.cache.lock().unwrap_or_else(|e| e.into_inner());
                    {
                        c.allowed = got; // a failed refresh drops to None
                    }
                    c.fetched = Some(Instant::now());
                    c.refreshing = false;
                });
            if spawned.is_err() {
                self.cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .refreshing = false;
            }
        }
        value
    }
}

const URL_ENV: &str = "P2C_TRAY_URL";
const TOKEN_ENV: &str = "P2C_TRAY_TOKEN";

/// `(url, token)` from the environment, removing both variables.
fn take_env() -> Option<(String, String)> {
    let url = std::env::var(URL_ENV).ok();
    let token = std::env::var(TOKEN_ENV).ok();
    std::env::remove_var(URL_ENV);
    std::env::remove_var(TOKEN_ENV);
    Some((url?, token?))
}

fn bad(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_string())
}

fn parse_response(raw: &[u8]) -> std::io::Result<(u16, Vec<u8>)> {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| bad("incomplete response"))?;
    let head = String::from_utf8_lossy(&raw[..end]).to_string();
    let body = &raw[end + 4..];
    let mut lines = head.lines();
    let code = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| bad("bad status line"))?;
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    if !chunked {
        return Ok((code, body.to_vec()));
    }
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let nl = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| bad("bad chunk"))?;
        let size_txt = String::from_utf8_lossy(&rest[..nl]).to_string();
        let size = usize::from_str_radix(size_txt.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| bad("bad chunk size"))?;
        rest = &rest[nl + 2..];
        if size == 0 {
            break;
        }
        if rest.len() < size + 2 {
            return Err(bad("short chunk"));
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
    Ok((code, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn tray_link_env_is_read_once_and_removed() {
        // Chromium helpers and ffmpeg must not inherit the tray token.
        std::env::set_var(URL_ENV, "http://127.0.0.1:8766");
        std::env::set_var(TOKEN_ENV, "sekret-token");
        let got = take_env();
        assert_eq!(got, Some(("http://127.0.0.1:8766".into(), "sekret-token".into())));
        assert!(std::env::var_os(URL_ENV).is_none());
        assert!(std::env::var_os(TOKEN_ENV).is_none());
        assert_eq!(take_env(), None);
    }

    /// Answers every connection with `reply`; sends each request's text to the channel.
    fn fake(reply: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let mut s = s.unwrap();
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
                let _ = s.write_all(reply.as_bytes());
                let _ = s.shutdown(std::net::Shutdown::Write);
                // Drain the rest of the request so the close isn't a reset.
                let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
                while s.read(&mut buf).is_ok_and(|n| n > 0) {}
            }
        });
        (url, rx)
    }

    #[test]
    fn forwards_with_bearer_and_returns_status_and_body() {
        let (url, rx) = fake("HTTP/1.1 409 Conflict\r\nContent-Length: 10\r\n\r\n{\"a\":true}");
        let link = TrayLink::new(&url, "sekret").unwrap();
        let (code, body) = link.request("POST", Some(b"{\"allowed\":true}")).unwrap();
        assert_eq!(code, 409);
        assert_eq!(body, b"{\"a\":true}");
        let req = rx.recv().unwrap();
        assert!(req.starts_with("POST /tray/agents HTTP/1.1"));
        assert!(req.contains("Authorization: Bearer sekret"));
    }

    #[test]
    fn decodes_chunked_bodies() {
        let (code, body) = parse_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!((code, body), (200, b"abcde".to_vec()));
    }

    #[test]
    fn allowed_is_cached_and_never_blocks() {
        let (url, _rx) =
            fake("HTTP/1.1 200 OK\r\nContent-Length: 28\r\n\r\n{\"allowed\":true,\"agents\":[]}");
        let link = TrayLink::new(&url, "t").unwrap();
        assert_eq!(link.allowed(), None, "first call returns at once");
        let start = Instant::now();
        while link.allowed() != Some(true) {
            assert!(start.elapsed() < Duration::from_secs(3), "never refreshed");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn rejects_non_http_urls() {
        assert!(TrayLink::new("https://x", "t").is_none());
        assert!(TrayLink::new("http://127.0.0.1:1/x", "t").is_none());
        assert!(TrayLink::new("http://127.0.0.1:1", "").is_none());
    }

    #[test]
    fn allowed_is_none_until_fetched_and_fails_closed() {
        // Answers `true` once, then 500.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for (i, s) in l.incoming().enumerate() {
                let mut s = s.unwrap();
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let reply: &[u8] = if i == 0 {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\n{\"allowed\":true}"
                } else {
                    b"HTTP/1.1 500 Oops\r\nContent-Length: 0\r\n\r\n"
                };
                let _ = s.write_all(reply);
            }
        });
        let link = TrayLink::with_limits(&url, "t", Duration::from_millis(50), TIMEOUT).unwrap();
        assert_eq!(link.allowed(), None, "unknown before the first fetch");
        let wait = |want: Option<bool>| {
            let start = Instant::now();
            while link.allowed() != want {
                assert!(start.elapsed() < Duration::from_secs(3), "never {want:?}");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        wait(Some(true));
        wait(None); // the next refresh fails: never keep a stale true
    }

    #[test]
    fn total_deadline_beats_slow_dribble() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for s in l.incoming() {
                let mut s = s.unwrap();
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\n");
                for _ in 0..100 {
                    std::thread::sleep(Duration::from_millis(50));
                    if s.write_all(b"x").is_err() {
                        break;
                    }
                }
            }
        });
        let link = TrayLink::with_limits(&url, "t", CACHE_FOR, Duration::from_millis(400)).unwrap();
        let start = Instant::now();
        assert!(link.request("GET", None).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn body_over_the_cap_is_an_error() {
        assert_eq!(read_limited(&b"abcd"[..], 4).unwrap(), b"abcd");
        assert!(read_limited(&b"abcde"[..], 4).is_err());
    }
}
