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
        .filter(|c| !is_own(c, own_device))
        .find(|c| {
            wanted
                .as_ref()
                .is_none_or(|w| c.name.to_lowercase().contains(w))
        })
        .cloned()
}

/// Find `original` again in a fresh device list (after a replug or a camera
/// service restart the enumeration index can point at another device, even at
/// our own output). Matches the exact name (case-insensitive); among same-named
/// devices the one with the original path wins; a unique name match is taken
/// even with a new path (USB replug on Linux). Never returns our own device.
pub fn reselect(
    original: &CameraInfo,
    cams: &[CameraInfo],
    own_device: Option<&Path>,
) -> Option<CameraInfo> {
    let named: Vec<&CameraInfo> = cams
        .iter()
        .filter(|c| !is_own(c, own_device))
        .filter(|c| c.name.to_lowercase() == original.name.to_lowercase())
        .collect();
    if let Some(orig_path) = original.path.as_deref() {
        if let Some(c) = named
            .iter()
            .find(|c| c.path.as_deref().is_some_and(|p| same_device(p, orig_path)))
        {
            return Some((*c).clone());
        }
    }
    match named.as_slice() {
        [only] => Some((*only).clone()),
        _ => None,
    }
}

/// Page2Cam's own output device: by name, or by device path.
fn is_own(c: &CameraInfo, own_device: Option<&Path>) -> bool {
    c.name
        .to_lowercase()
        .contains(&p2c_shm::CAMERA_NAME.to_lowercase())
        || own_device.is_some_and(|own| c.path.as_deref().is_some_and(|p| same_device(p, own)))
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
    fn reselect_finds_renumbered_camera_by_name() {
        let original = cam(0, "Integrated Camera", None);
        let cams = [
            cam(0, "Logitech C920", None),
            cam(1, "integrated camera", None),
        ];
        assert_eq!(reselect(&original, &cams, None).unwrap().index, 1);
    }

    #[test]
    fn reselect_never_takes_own_camera_at_old_index() {
        let original = cam(0, "Integrated Camera", Some("/dev/video0"));
        let cams = [
            cam(0, "Page2Cam", Some("/dev/video0")),
            cam(1, "Integrated Camera", Some("/dev/video2")),
        ];
        let got = reselect(&original, &cams, Some(Path::new("/dev/video0"))).unwrap();
        assert_eq!(got.index, 1);
    }

    #[test]
    fn reselect_never_takes_own_device_even_if_name_matches() {
        let original = cam(0, "Dummy video device", Some("/dev/video0"));
        let cams = [cam(0, "Dummy video device", Some("/dev/video5"))];
        assert!(reselect(&original, &cams, Some(Path::new("/dev/video5"))).is_none());
    }

    #[test]
    fn reselect_none_when_camera_missing() {
        let original = cam(1, "Logitech C920", None);
        let cams = [cam(0, "Integrated Camera", None), cam(1, "Page2Cam", None)];
        assert!(reselect(&original, &cams, None).is_none());
    }

    #[test]
    fn reselect_never_takes_page2cam_named_device() {
        // Even if the operator's camera were literally named like ours.
        let original = cam(0, "page2cam", None);
        assert!(reselect(&original, &[cam(0, "Page2Cam", None)], None).is_none());
    }

    #[test]
    fn reselect_prefers_same_path_among_same_names() {
        let original = cam(0, "USB Camera", Some("/dev/video2"));
        let cams = [
            cam(0, "USB Camera", Some("/dev/video0")),
            cam(1, "USB Camera", Some("/dev/video2")),
        ];
        assert_eq!(reselect(&original, &cams, None).unwrap().index, 1);
    }

    #[test]
    fn reselect_ambiguous_names_without_path_match_is_none() {
        let original = cam(0, "USB Camera", Some("/dev/video9"));
        let cams = [
            cam(0, "USB Camera", Some("/dev/video0")),
            cam(1, "USB Camera", Some("/dev/video2")),
        ];
        assert!(reselect(&original, &cams, None).is_none());
    }

    #[test]
    fn reselect_unique_name_survives_new_path() {
        // USB replug on Linux: same camera, new /dev/videoN.
        let original = cam(0, "Logitech C920", Some("/dev/video0"));
        let cams = [cam(3, "Logitech C920", Some("/dev/video4"))];
        assert_eq!(reselect(&original, &cams, None).unwrap().index, 3);
    }

    #[test]
    fn picker_none_when_empty() {
        assert!(pick_camera(&[], None, None).is_none());
    }
}
