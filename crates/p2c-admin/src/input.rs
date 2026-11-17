/// Map a preview position (0..1 of the preview) to page pixels, clamped.
pub fn map_preview(x: f64, y: f64, width: u32, height: u32) -> (i32, i32) {
    let f = |v: f64, n: u32| {
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        ((v * n as f64) as i64).clamp(0, n.saturating_sub(1) as i64) as i32
    };
    (f(x, width), f(y, height))
}

/// Accept only web-ish URLs; a bare host gets `https://`. Rejects `javascript:`,
/// `chrome://`, `data:` and similar so the UI can't be used to run script URLs.
pub fn normalize_url(input: &str) -> anyhow::Result<String> {
    let s = input.trim();
    anyhow::ensure!(!s.is_empty(), "empty URL");
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("file://")
        || lower == "about:blank"
    {
        return Ok(s.to_string());
    }
    // Anything else with a scheme ("x:" before any '/' or '.') is refused.
    let scheme_end = s.find(':');
    let has_scheme = scheme_end.is_some_and(|i| {
        !s[..i].contains(['/', '.']) && !s[i + 1..].starts_with(|c: char| c.is_ascii_digit())
    });
    anyhow::ensure!(!has_scheme, "only http, https and file URLs are allowed");
    anyhow::ensure!(!s.contains(char::is_whitespace), "URL contains spaces");
    Ok(format!("https://{s}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_preview_coords() {
        assert_eq!(map_preview(0.5, 0.5, 1920, 1080), (960, 540));
        assert_eq!(map_preview(0.0, 0.0, 1920, 1080), (0, 0));
        assert_eq!(map_preview(1.0, 1.0, 1920, 1080), (1919, 1079));
        assert_eq!(map_preview(-0.2, 7.0, 1920, 1080), (0, 1079));
        assert_eq!(map_preview(f64::NAN, 0.5, 1920, 1080), (0, 540));
    }

    #[test]
    fn navigate_rejects_non_http_scheme() {
        assert!(normalize_url("javascript:alert(1)").is_err());
        assert!(normalize_url("chrome://settings").is_err());
        assert!(normalize_url("  ").is_err());
        assert_eq!(normalize_url("example.com").unwrap(), "https://example.com");
        assert_eq!(
            normalize_url(" http://x.test/a?b ").unwrap(),
            "http://x.test/a?b"
        );
        assert_eq!(normalize_url("about:blank").unwrap(), "about:blank");
        assert_eq!(
            normalize_url("file:///tmp/a.html").unwrap(),
            "file:///tmp/a.html"
        );
    }
}
