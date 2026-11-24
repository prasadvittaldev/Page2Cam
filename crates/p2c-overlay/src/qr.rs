//! QR codes for `qr` template parameters: the engine encodes the text and
//! passes the dark modules as SVG path data, so templates need no QR code
//! of their own.
use std::fmt::Write as _;

use anyhow::{anyhow, Result};
use qrcode::{Color, EcLevel, QrCode};

/// The module count and an SVG path (`M x y h… v1 h-… z` per run of dark
/// modules, in module units) for `text`.
pub fn svg_path(text: &str) -> Result<(usize, String)> {
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M)
        .map_err(|e| anyhow!("cannot make a QR code: {e}"))?;
    let n = code.width();
    let colors = code.to_colors();
    let mut d = String::new();
    for y in 0..n {
        let mut x = 0;
        while x < n {
            if colors[y * n + x] == Color::Dark {
                let s = x;
                while x < n && colors[y * n + x] == Color::Dark {
                    x += 1;
                }
                let _ = write!(d, "M{s} {y}h{}v1h-{}z", x - s, x - s);
            } else {
                x += 1;
            }
        }
    }
    Ok((n, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_link() {
        let (n, d) = svg_path("https://example.com").unwrap();
        assert_eq!(n, 25, "version 2 at level M");
        assert!(d.starts_with("M0 0h7v1h-7z"), "finder pattern first: {d}");
        assert!(svg_path(&"x".repeat(5000)).is_err());
    }
}
