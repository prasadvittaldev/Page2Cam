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

struct Cache {
    allowed: bool,
    fetched: Option<Instant>,
    refreshing: bool,
}

pub struct TrayLink {
    host_port: String,
    token: String,
    cache: Mutex<Cache>,
}

impl TrayLink {
    /// `url` is `http://host:port`; anything else is refused.
    pub fn new(url: &str, token: &str) -> Option<Arc<TrayLink>> {
        let rest = url.trim().strip_prefix("http://")?;
        let host_port = rest.trim_end_matches('/');
        if host_port.is_empty() || host_port.contains('/') || token.is_empty() {
            return None;
        }
        Some(Arc::new(TrayLink {
            host_port: host_port.to_string(),
            token: token.to_string(),
            cache: Mutex::new(Cache {
                allowed: false,
                fetched: None,
                refreshing: false,
            }),
        }))
    }

    /// The link from the environment, read once; `None` when standalone.
    pub fn global() -> Option<Arc<TrayLink>> {
        static LINK: OnceLock<Option<Arc<TrayLink>>> = OnceLock::new();
        LINK.get_or_init(|| {
            let url = std::env::var("P2C_TRAY_URL").ok()?;
            let token = std::env::var("P2C_TRAY_TOKEN").ok()?;
            TrayLink::new(&url, &token)
        })
        .clone()
    }

    /// One request to `/tray/agents`. Returns the tray's status and body.
    pub fn request(&self, method: &str, body: Option<&[u8]>) -> std::io::Result<(u16, Vec<u8>)> {
        let addr = self
            .host_port
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| std::io::Error::other("tray address does not resolve"))?;
        let mut s = TcpStream::connect_timeout(&addr, TIMEOUT)?;
        s.set_read_timeout(Some(TIMEOUT))?;
        s.set_write_timeout(Some(TIMEOUT))?;
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
        let mut raw = Vec::new();
        s.take(4 * 1024 * 1024).read_to_end(&mut raw)?;
        parse_response(&raw)
    }

    /// Last known "agents allowed", never blocking: a stale value triggers a
    /// background refresh and the old value is returned meanwhile.
    pub fn allowed(self: &Arc<Self>) -> bool {
        let (value, refresh) = {
            let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            let stale = c.fetched.is_none_or(|t| t.elapsed() > CACHE_FOR);
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
                    if let Some(a) = got {
                        c.allowed = a;
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
        assert!(!link.allowed(), "first call returns the default at once");
        let start = Instant::now();
        while !link.allowed() {
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
}
