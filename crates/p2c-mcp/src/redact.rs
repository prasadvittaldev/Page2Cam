//! Keeps secrets out of anything an agent sees: stream keys in RTMP URLs,
//! tokens in query strings, headers and JSON.

const MASK: &str = "***";

/// `s` with RTMP stream keys, `t=` query values, `Bearer` tokens,
/// `X-P2C-Token` headers and JSON `"token"`/`"key"` values replaced by `***`.
pub fn redact(s: &str) -> String {
    let s = redact_rtmp(s);
    let s = redact_after(&s, &["?t=", "&t="], |c| c == '&' || end_char(c));
    let s = redact_after_ci(&s, &["bearer "]);
    let s = redact_after_ci(&s, &["x-p2c-token: ", "x-p2c-token:"]);
    redact_json_keys(&s, &["token", "key", "agent_token", "stream_key"])
}

fn end_char(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ',' | ')' | ']' | '}' | '\\')
}

/// Replaces everything after the app segment of `rtmp(s)://host/app/…`.
fn redact_rtmp(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(i) = [lower.find("rtmp://"), lower.find("rtmps://")]
            .into_iter()
            .flatten()
            .min()
        else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..i]);
        let url_start = &rest[i..];
        let end = url_start.find(end_char).unwrap_or(url_start.len());
        let url = &url_start[..end];
        let scheme_end = url.find("://").map(|p| p + 3).unwrap_or(0);
        let after = &url[scheme_end..];
        // host / app / key...
        let mut parts = after.splitn(3, '/');
        let host = parts.next().unwrap_or("");
        let app = parts.next();
        let key = parts.next();
        out.push_str(&url[..scheme_end]);
        // user:pass@host
        match host.rsplit_once('@') {
            Some((_, h)) => {
                out.push_str(MASK);
                out.push('@');
                out.push_str(h.split('?').next().unwrap_or(""));
            }
            None => out.push_str(host.split('?').next().unwrap_or("")),
        }
        if let Some(app) = app {
            out.push('/');
            let (a, q) = app.split_once('?').map_or((app, false), |(a, _)| (a, true));
            out.push_str(a);
            if q {
                out.push('?');
                out.push_str(MASK);
            }
        }
        if key.is_some_and(|k| !k.is_empty()) {
            out.push('/');
            out.push_str(MASK);
        }
        rest = &url_start[end..];
    }
}

/// Masks the value after each (case-sensitive, ASCII) marker up to `stop`.
fn redact_after(s: &str, markers: &[&str], stop: impl Fn(char) -> bool) -> String {
    let mut out = s.to_string();
    for m in markers {
        let mut from = 0;
        while let Some(p) = out[from..].find(m) {
            // Markers are ASCII, so `start` is a char boundary.
            let start = from + p + m.len();
            let len = out[start..].find(&stop).unwrap_or(out.len() - start);
            if len > 0 {
                out.replace_range(start..start + len, MASK);
                from = start + MASK.len();
            } else {
                from = start;
            }
        }
    }
    out
}

/// Masks the word after each case-insensitive (ASCII) marker.
fn redact_after_ci(s: &str, markers: &[&str]) -> String {
    let mut out = s.to_string();
    for m in markers {
        let mut from = 0;
        loop {
            // ASCII lowercasing keeps byte offsets.
            let lower = out.to_ascii_lowercase();
            let Some(p) = lower[from..].find(m) else {
                break;
            };
            let start = from + p + m.len();
            let len = out[start..].find(end_char).unwrap_or(out.len() - start);
            if len > 0 {
                out.replace_range(start..start + len, MASK);
                from = start + MASK.len();
            } else {
                from = start;
            }
        }
    }
    out
}

/// Masks JSON string values of the given keys (`"token": "x"` → `"token": "***"`).
fn redact_json_keys(s: &str, keys: &[&str]) -> String {
    let mut out = s.to_string();
    for k in keys {
        let needle = format!("\"{k}\"");
        let mut from = 0;
        while let Some(p) = out[from..].find(&needle) {
            let mut i = from + p + needle.len();
            let bytes = out.as_bytes();
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\n' || bytes[i] == b'\r') {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b':' {
                i += 1;
                while i < bytes.len()
                    && (bytes[i] == b' ' || bytes[i] == b'\n' || bytes[i] == b'\r')
                {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'"' {
                    let start = i + 1;
                    // Find the closing quote, skipping escapes.
                    let mut j = start;
                    while j < bytes.len() && bytes[j] != b'"' {
                        if bytes[j] == b'\\' {
                            j += 1;
                        }
                        j += 1;
                    }
                    let j = j.min(bytes.len());
                    if j > start {
                        out.replace_range(start..j, MASK);
                    }
                    i = start + MASK.len();
                }
            }
            from = i.min(out.len());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtmp_keys_are_masked() {
        let r = redact("error: rtmp://a.rtmp.youtube.com/live2/abcd-1234 refused");
        assert_eq!(r, "error: rtmp://a.rtmp.youtube.com/live2/*** refused");
        let r = redact("RTMPS://x.example:443/app/k/ey?x=1");
        assert!(!r.contains("ey?x"), "{r}");
        let r = redact("rtmp://user:pw@host/app/key");
        assert!(!r.contains("pw") && !r.contains("/key"), "{r}");
        // Server only: unchanged.
        assert_eq!(
            redact("rtmp://a.rtmp.youtube.com/live2"),
            "rtmp://a.rtmp.youtube.com/live2"
        );
    }

    #[test]
    fn tokens_are_masked() {
        assert_eq!(
            redact("http://127.0.0.1:1/?t=SECRET&x=1"),
            "http://127.0.0.1:1/?t=***&x=1"
        );
        assert_eq!(redact("http://h/?a=1&t=SECRET"), "http://h/?a=1&t=***");
        assert_eq!(
            redact("Authorization: Bearer SECRET"),
            "Authorization: Bearer ***"
        );
        assert_eq!(
            redact("authorization: bearer SECRET"),
            "authorization: bearer ***"
        );
        assert_eq!(redact("X-P2C-Token: SECRET"), "X-P2C-Token: ***");
        assert_eq!(
            redact(r#"{"token": "SECRET", "a": 1}"#),
            r#"{"token": "***", "a": 1}"#
        );
        assert_eq!(redact(r#"{"key":"SECRET"}"#), r#"{"key":"***"}"#);
        // Not a token: unchanged.
        assert_eq!(redact("format=json&tag=x"), "format=json&tag=x");
    }

    #[test]
    fn redaction_never_panics_on_odd_text() {
        for s in [
            "",
            "?t=",
            "&t=",
            "Bearer",
            "bearer ",
            "rtmp://",
            "rtmps://h/",
            "\"token\"",
            "\"token\":",
            "\"token\":\"",
            "é?t=é",
            "rtmp://é/é/é",
        ] {
            let _ = redact(s);
        }
    }
}
