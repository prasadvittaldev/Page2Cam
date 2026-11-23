/// `#rrggbb` (case-insensitive) as `[r, g, b]`.
pub fn parse_colour(s: &str) -> Option<[u8; 3]> {
    let hex = s.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// A colour in its saved form (`#rrggbb`, lower case), or an error message.
pub fn normalize_colour(s: &str) -> Result<String, String> {
    let [r, g, b] =
        parse_colour(s).ok_or_else(|| format!("{:?} is not a colour (use #rrggbb)", s.trim()))?;
    Ok(format!("#{r:02x}{g:02x}{b:02x}"))
}

pub fn is_video_id(s: &str) -> bool {
    s.len() == 11
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The 11-character video id of a YouTube link (watch, youtu.be, shorts,
/// embed, live) or of a bare id. Timestamps and playlist parameters are
/// ignored; anything else is refused with a message for the operator.
pub fn youtube_id(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("paste a YouTube video link".into());
    }
    if is_video_id(s) {
        return Ok(s.to_string());
    }
    let not_video = || {
        Err::<String, String>(format!(
            "{s:?} is not a YouTube video link (use a link like https://www.youtube.com/watch?v=…)"
        ))
    };
    let lower = s.to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &s[8..]
    } else if lower.starts_with("http://") {
        &s[7..]
    } else if lower.contains("://") {
        return not_video();
    } else {
        s
    };
    // host / path ? query # fragment
    let rest = rest.split('#').next().unwrap_or("");
    let (host_part, path_query) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = host_part.to_ascii_lowercase();
    let host = host.split(':').next().unwrap_or("");
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("m."))
        .or_else(|| host.strip_prefix("music."))
        .unwrap_or(host);
    let (path, query) = match path_query.find('?') {
        Some(i) => (&path_query[..i], &path_query[i + 1..]),
        None => (path_query, ""),
    };
    let segments: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let checked = |id: &str| {
        if is_video_id(id) {
            Ok(id.to_string())
        } else {
            Err(format!("{s:?} has no valid YouTube video id"))
        }
    };
    match host {
        "youtu.be" => match segments.first() {
            Some(id) => checked(id),
            None => not_video(),
        },
        "youtube.com" | "youtube-nocookie.com" => match segments.as_slice() {
            ["watch"] => match query.split('&').find_map(|kv| kv.strip_prefix("v=")) {
                Some(id) => checked(id),
                None => Err(format!("{s:?} has no video id (no v=…)")),
            },
            ["shorts" | "embed" | "live" | "v", id, ..] => checked(id),
            ["playlist", ..] => {
                Err("a playlist link has no single video: open one video and copy its link".into())
            }
            _ => not_video(),
        },
        _ => not_video(),
    }
}
