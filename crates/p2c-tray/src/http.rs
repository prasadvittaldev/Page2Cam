//! A tiny blocking HTTP/1.1 client for the engine's admin API on
//! 127.0.0.1 (`GET /api/state`, `POST /api/quit`). Every request carries the
//! token header and the `Host` the engine's DNS-rebinding guard expects.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use p2c_admin::Health;

/// Per-request limit (connect, then read/write each).
pub const TIMEOUT: Duration = Duration::from_millis(1500);
/// Answers larger than this are refused (the state JSON is a few KB).
const MAX_RESPONSE: u64 = 1 << 20;

/// One request; returns (status code, body).
pub fn request(port: u16, method: &str, path: &str, token: &str) -> anyhow::Result<(u16, String)> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut s = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    s.set_read_timeout(Some(TIMEOUT))?;
    s.set_write_timeout(Some(TIMEOUT))?;
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-P2C-Token: {token}\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes())?;
    let mut raw = Vec::new();
    s.take(MAX_RESPONSE).read_to_end(&mut raw)?;
    parse_response(&raw)
}

/// One request with an optional body (`content type`, bytes) and an overall
/// `timeout`; returns (status, body bytes). Bodies above `max` are an error.
pub fn request_full(
    port: u16,
    method: &str,
    path: &str,
    token: &str,
    body: Option<(&str, &[u8])>,
    timeout: Duration,
    max: u64,
) -> anyhow::Result<(u16, Vec<u8>)> {
    let deadline = std::time::Instant::now() + timeout;
    let left = || {
        deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| anyhow::anyhow!("Page2Cam did not answer in {} s", timeout.as_secs()))
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut s = TcpStream::connect_timeout(&addr, left()?.min(TIMEOUT))?;
    s.set_write_timeout(Some(left()?))?;
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-P2C-Token: {token}\r\nConnection: close\r\n"
    );
    let bytes = match body {
        Some((kind, b)) => {
            head.push_str(&format!(
                "Content-Type: {kind}\r\nContent-Length: {}\r\n",
                b.len()
            ));
            b
        }
        None => {
            head.push_str("Content-Length: 0\r\n");
            &[][..]
        }
    };
    head.push_str("\r\n");
    s.write_all(head.as_bytes())?;
    s.write_all(bytes)?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        s.set_read_timeout(Some(left()?))?;
        let n = s.read(&mut buf)?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        anyhow::ensure!(
            raw.len() as u64 <= max + 64 * 1024,
            "the answer is too large"
        );
    }
    parse_response_bytes(&raw)
}

/// [`parse_response`] for binary bodies.
pub fn parse_response_bytes(raw: &[u8]) -> anyhow::Result<(u16, Vec<u8>)> {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("incomplete HTTP response"))?;
    let head = String::from_utf8_lossy(&raw[..end]);
    let body = &raw[end + 4..];
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("bad HTTP status line"))?;
    let chunked = lines.any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            k.trim().eq_ignore_ascii_case("transfer-encoding")
                && v.trim().eq_ignore_ascii_case("chunked")
        })
    });
    if !chunked {
        return Ok((status, body.to_vec()));
    }
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let nl = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| anyhow::anyhow!("bad chunk"))?;
        let size_text = String::from_utf8_lossy(&rest[..nl]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)?;
        rest = &rest[nl + 2..];
        if size == 0 {
            return Ok((status, out));
        }
        anyhow::ensure!(rest.len() >= size, "short chunk");
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size..];
        rest = rest.strip_prefix(b"\r\n").unwrap_or(rest);
    }
}

/// Split a full HTTP/1.1 response into status and body (handles chunked).
pub fn parse_response(raw: &[u8]) -> anyhow::Result<(u16, String)> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("incomplete HTTP response"))?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("bad HTTP status line"))?;
    let chunked = lines.any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            k.trim().eq_ignore_ascii_case("transfer-encoding")
                && v.trim().eq_ignore_ascii_case("chunked")
        })
    });
    let body = if chunked {
        dechunk(body)?
    } else {
        body.to_string()
    };
    Ok((status, body))
}

fn dechunk(mut s: &str) -> anyhow::Result<String> {
    let mut out = String::new();
    loop {
        let (size, rest) = s
            .split_once("\r\n")
            .ok_or_else(|| anyhow::anyhow!("bad chunk"))?;
        let size = usize::from_str_radix(size.split(';').next().unwrap_or("").trim(), 16)?;
        if size == 0 {
            return Ok(out);
        }
        anyhow::ensure!(rest.len() >= size, "short chunk");
        out.push_str(&rest[..size]);
        s = rest[size..].trim_start_matches("\r\n");
    }
}

/// `health` from the engine's `/api/state` JSON (`{"status": {"health": …}}`).
pub fn parse_health(body: &str) -> anyhow::Result<Health> {
    let v: serde_json::Value = serde_json::from_str(body)?;
    let h = v
        .get("status")
        .and_then(|s| s.get("health"))
        .ok_or_else(|| anyhow::anyhow!("no status.health in /api/state"))?;
    Ok(serde_json::from_value(h.clone())?)
}

/// The engine's health, or why it could not be read.
pub fn get_health(port: u16, token: &str) -> anyhow::Result<Health> {
    let (code, body) = request(port, "GET", "/api/state", token)?;
    anyhow::ensure!(code == 200, "/api/state answered {code}");
    parse_health(&body)
}

/// Ask the engine to shut down normally.
pub fn post_quit(port: u16, token: &str) -> anyhow::Result<()> {
    let (code, _) = request(port, "POST", "/api/quit", token)?;
    anyhow::ensure!(code == 200, "/api/quit answered {code}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    /// Serves one request; hands the request head back for inspection.
    fn serve_once(answer: &'static str) -> (u16, std::thread::JoinHandle<String>) {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut head = String::new();
            let mut r = std::io::BufReader::new(s.try_clone().unwrap());
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                head.push_str(&line);
            }
            s.write_all(answer.as_bytes()).unwrap();
            // Close gracefully (FIN, then wait for the client's close):
            // dropping at once can make Windows send a reset that the
            // client sees before the answer (os error 10054).
            let _ = s.shutdown(std::net::Shutdown::Write);
            let _ = std::io::copy(&mut r, &mut std::io::sink());
            head
        });
        (port, t)
    }

    #[test]
    fn get_health_sends_token_and_host_and_reads_chunked_json() {
        let (port, t) = serve_once(
            "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\ncontent-type: application/json\r\n\r\n\
             1b\r\n{\"status\":{\"health\":{\"outpu\r\n\
             16\r\nt_ready\":true}},\"x\":1}\r\n0\r\n\r\n",
        );
        let h = get_health(port, "tok123").unwrap();
        assert!(h.output_ready);
        let head = t.join().unwrap();
        assert!(head.starts_with("GET /api/state HTTP/1.1\r\n"), "{head}");
        assert!(
            head.contains(&format!("Host: 127.0.0.1:{port}\r\n")),
            "{head}"
        );
        assert!(head.contains("X-P2C-Token: tok123\r\n"), "{head}");
    }

    #[test]
    fn quit_needs_200() {
        let (port, t) = serve_once("HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n");
        assert!(post_quit(port, "t").is_err());
        assert!(t.join().unwrap().starts_with("POST /api/quit HTTP/1.1"));
        let (port, t) = serve_once("HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
        post_quit(port, "t").unwrap();
        t.join().unwrap();
    }

    #[test]
    fn health_parse_errors() {
        assert!(parse_health("{}").is_err());
        assert!(parse_health("nope").is_err());
        let h = parse_health(r#"{"status":{"health":{}}}"#).unwrap();
        assert!(!h.output_ready);
        assert!(parse_response(b"HTTP/1.1 200 OK\r\n").is_err());
    }

    #[test]
    fn closed_port_is_an_error() {
        let port = crate::launch::pick_free_port().unwrap();
        assert!(get_health(port, "t").is_err());
    }
}
