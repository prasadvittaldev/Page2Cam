use std::path::{Path, PathBuf};

/// A capture device as seen by Page2Cam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraInfo {
    pub index: u32,
    pub name: String,
    /// Device node (e.g. /dev/video0) when the platform exposes one.
    pub path: Option<PathBuf>,
}

/// Choose the webcam to overlay. Never returns Page2Cam's own output device
/// (by path or by name), which would feed the output back into itself.
pub fn pick_camera(
    cams: &[CameraInfo],
    wanted: Option<&str>,
    own_device: Option<&Path>,
) -> Option<CameraInfo> {
    let wanted = wanted.map(str::to_lowercase);
    cams.iter()
        .filter(|c| !c.name.contains("Page2Cam"))
        .filter(|c| {
            own_device.is_none_or(|own| c.path.as_deref().is_none_or(|p| !same_device(p, own)))
        })
        .find(|c| {
            wanted
                .as_ref()
                .is_none_or(|w| c.name.to_lowercase().contains(w))
        })
        .cloned()
}

/// Same device node, resolving symlinks (e.g. /dev/v4l/by-id/...) when possible.
fn same_device(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam(index: u32, name: &str, path: Option<&str>) -> CameraInfo {
        CameraInfo {
            index,
            name: name.into(),
            path: path.map(PathBuf::from),
        }
    }

    #[test]
    fn picker_skips_own_loopback() {
        let cams = [
            cam(0, "Page2Cam", Some("/dev/video5")),
            cam(1, "Integrated Camera", Some("/dev/video0")),
        ];
        let got = pick_camera(&cams, None, Some(Path::new("/dev/video5"))).unwrap();
        assert_eq!(got.index, 1);
    }

    #[test]
    fn picker_skips_own_device_even_with_other_name() {
        let cams = [cam(0, "Dummy video device", Some("/dev/video5"))];
        assert!(pick_camera(&cams, None, Some(Path::new("/dev/video5"))).is_none());
    }

    #[test]
    fn picker_skips_by_name_even_without_path() {
        assert!(pick_camera(&[cam(0, "Page2Cam", None)], None, None).is_none());
    }

    #[test]
    fn picker_honors_wanted_name() {
        let cams = [
            cam(0, "Integrated Camera", None),
            cam(1, "Logitech C920", None),
        ];
        assert_eq!(pick_camera(&cams, Some("c920"), None).unwrap().index, 1);
    }

    #[cfg(unix)]
    #[test]
    fn picker_skips_own_device_via_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("video5");
        std::fs::write(&real, b"").unwrap();
        let link = dir.path().join("by-id-page2cam");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let cams = [cam(0, "Dummy video device", Some(real.to_str().unwrap()))];
        assert!(pick_camera(&cams, None, Some(&link)).is_none());
    }

    #[test]
    fn picker_none_when_empty() {
        assert!(pick_camera(&[], None, None).is_none());
    }
}
