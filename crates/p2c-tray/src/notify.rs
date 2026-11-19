//! Balloon notifications. Windows shows one balloon per icon and a new one
//! replaces the one on screen, so the shell queues them and shows what
//! piled up meanwhile as one merged balloon.

/// One balloon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Balloon {
    pub text: String,
    pub warning: bool,
}

/// The balloon's text limit (`szInfo` holds 256 UTF-16 units with the NUL).
pub const TEXT_MAX: usize = 255;

/// Merge pending balloons into one: texts in order without repeats, one per
/// line, clipped to [`TEXT_MAX`]; a warning if any was one. `None` if empty.
pub fn merge(pending: &[Balloon]) -> Option<Balloon> {
    if pending.is_empty() {
        return None;
    }
    let mut texts: Vec<&str> = Vec::new();
    for p in pending {
        if !texts.contains(&p.text.as_str()) {
            texts.push(&p.text);
        }
    }
    let mut text = texts.join("\n");
    if text.encode_utf16().count() > TEXT_MAX {
        let mut out = String::new();
        let mut n = 0;
        for c in text.chars() {
            if n + c.len_utf16() > TEXT_MAX - 1 {
                break;
            }
            n += c.len_utf16();
            out.push(c);
        }
        out.push('…');
        text = out;
    }
    Some(Balloon {
        text,
        warning: pending.iter().any(|p| p.warning),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(text: &str, warning: bool) -> Balloon {
        Balloon {
            text: text.into(),
            warning,
        }
    }

    #[test]
    fn pending_balloons_merge_into_one() {
        assert_eq!(merge(&[]), None);
        assert_eq!(merge(&[b("a", false)]), Some(b("a", false)));
        let m = merge(&[
            b("Page2Cam crashed - restarting", true),
            b("Page2Cam stopped", false),
            b("Page2Cam crashed - restarting", true),
        ])
        .unwrap();
        assert_eq!(m.text, "Page2Cam crashed - restarting\nPage2Cam stopped");
        assert!(m.warning);
        let long = merge(&[b(&"x".repeat(200), false), b(&"y".repeat(200), false)]).unwrap();
        assert_eq!(long.text.encode_utf16().count(), TEXT_MAX);
        assert!(long.text.ends_with('…'));
    }
}
