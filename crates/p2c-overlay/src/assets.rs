//! Stored copies of overlay pictures and videos (`<data dir>/overlays/assets/`).
//! Files are named by us (`<layer id>-<random>.<ext>`), recognised by their
//! first bytes (never by a given name), and only ever served by that name.
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};

/// Largest stored picture.
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
/// Largest stored video (files given by path only).
pub const MAX_VIDEO_BYTES: u64 = 200 * 1024 * 1024;

/// What a stored file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

/// The extension and content type of `bytes`, by their first bytes.
pub fn sniff(bytes: &[u8]) -> Option<(MediaKind, &'static str, &'static str)> {
    let b = bytes;
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some((MediaKind::Image, "png", "image/png"))
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some((MediaKind::Image, "jpg", "image/jpeg"))
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some((MediaKind::Image, "webp", "image/webp"))
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some((MediaKind::Image, "gif", "image/gif"))
    } else if b.len() >= 12 && &b[4..8] == b"ftyp" {
        Some((MediaKind::Video, "mp4", "video/mp4"))
    } else if b.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        Some((MediaKind::Video, "webm", "video/webm"))
    } else {
        None
    }
}

/// The content type of a stored file name (by our own extension).
pub fn content_type(name: &str) -> Option<&'static str> {
    Some(match name.rsplit_once('.')?.1 {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => return None,
    })
}

/// A name we could have made: `[a-z0-9-]+.<known ext>`, no paths.
pub fn is_asset_name(name: &str) -> bool {
    let Some((stem, _)) = name.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && name.len() <= 80
        && stem
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && content_type(name).is_some()
}

/// The asset folder.
#[derive(Clone, Debug)]
pub struct Assets {
    dir: PathBuf,
}

impl Assets {
    pub fn new(dir: impl Into<PathBuf>) -> Assets {
        Assets { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Store `bytes` for layer `id`, which must be `want`. Returns the new
    /// file name.
    pub fn store(&self, id: &str, bytes: &[u8], want: MediaKind) -> Result<String> {
        let Some((kind, ext, _)) = sniff(bytes) else {
            bail!("not a supported file (pictures: PNG, JPEG, WebP, GIF; videos: MP4, WebM)");
        };
        ensure!(
            kind == want,
            "{} given where {} is needed",
            describe(kind),
            describe(want)
        );
        match kind {
            MediaKind::Image => {
                ensure!(bytes.len() <= MAX_IMAGE_BYTES, "picture larger than 20 MB")
            }
            MediaKind::Video => ensure!(
                bytes.len() as u64 <= MAX_VIDEO_BYTES,
                "video larger than 200 MB"
            ),
        }
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let name = format!("{id}-{}.{ext}", random_suffix());
        let path = self.dir.join(&name);
        let tmp = self.dir.join(format!("{name}.part"));
        std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("storing {}", path.display()))?;
        Ok(name)
    }

    /// Read a file the agent or operator pointed at (once, at upload) and
    /// store a copy. Videos up to 200 MB, pictures up to 20 MB.
    pub fn store_path(&self, id: &str, path: &Path, want: MediaKind) -> Result<String> {
        let meta =
            std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
        ensure!(meta.is_file(), "{} is not a file", path.display());
        let limit = match want {
            MediaKind::Image => MAX_IMAGE_BYTES as u64,
            MediaKind::Video => MAX_VIDEO_BYTES,
        };
        ensure!(
            meta.len() <= limit,
            "{} is larger than {} MB",
            path.display(),
            limit / (1024 * 1024)
        );
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        self.store(id, &bytes, want)
    }

    /// A stored file and its content type (`None` for unknown names).
    pub fn read(&self, name: &str) -> Option<(Vec<u8>, &'static str)> {
        if !is_asset_name(name) {
            return None;
        }
        let bytes = std::fs::read(self.dir.join(name)).ok()?;
        Some((bytes, content_type(name)?))
    }

    /// Delete a stored file (missing is fine).
    pub fn remove(&self, name: &str) {
        if is_asset_name(name) {
            let _ = std::fs::remove_file(self.dir.join(name));
        }
    }

    /// Delete stored files no layer uses any more.
    pub fn prune(&self, keep: &[&str]) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if is_asset_name(&name) && !keep.contains(&name.as_str()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn describe(k: MediaKind) -> &'static str {
    match k {
        MediaKind::Image => "a picture",
        MediaKind::Video => "a video",
    }
}

fn random_suffix() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..8)
        .map(|_| format!("{:x}", rng.random_range(0..16u8)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n0000";

    #[test]
    fn sniffs_by_content_only() {
        assert_eq!(sniff(PNG).unwrap().1, "png");
        assert_eq!(sniff(b"GIF89a....").unwrap().1, "gif");
        assert_eq!(
            sniff(b"\0\0\0\x20ftypisom0000").unwrap().0,
            MediaKind::Video
        );
        assert_eq!(sniff(&[0x1a, 0x45, 0xdf, 0xa3, 0]).unwrap().1, "webm");
        assert!(sniff(b"<html>").is_none());
    }

    #[test]
    fn names_are_ours_only() {
        assert!(is_asset_name("title-1-0a1b2c3d.png"));
        for bad in [
            "../x.png",
            "a/b.png",
            "x.exe",
            "X.png",
            ".png",
            "a.png.part",
            "a",
        ] {
            assert!(!is_asset_name(bad), "{bad}");
        }
    }

    #[test]
    fn store_read_remove_prune() {
        let d = tempfile::tempdir().unwrap();
        let a = Assets::new(d.path().join("assets"));
        let n = a.store("logo", PNG, MediaKind::Image).unwrap();
        assert!(n.starts_with("logo-") && n.ends_with(".png"), "{n}");
        assert_eq!(a.read(&n).unwrap(), (PNG.to_vec(), "image/png"));
        assert!(a.store("v", PNG, MediaKind::Video).is_err(), "kind checked");
        assert!(a.store("v", b"text", MediaKind::Image).is_err());
        assert!(a.read("../assets").is_none());
        let m = a.store("other", PNG, MediaKind::Image).unwrap();
        a.prune(&[&n]);
        assert!(a.read(&m).is_none() && a.read(&n).is_some());
        a.remove(&n);
        assert!(a.read(&n).is_none());
        let p = d.path().join("given.png");
        std::fs::write(&p, PNG).unwrap();
        let s = a.store_path("img", &p, MediaKind::Image).unwrap();
        assert!(a.read(&s).is_some());
        assert!(a.store_path("img", d.path(), MediaKind::Image).is_err());
    }
}
