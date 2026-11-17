//! v4l2loopback output sink. Uses a single raw VIDIOC_S_FMT ioctl instead of a
//! bindgen-based crate so building needs no libclang.
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use anyhow::Context;
use p2c_compositor::{FrameSink, OutputSpec};
use p2c_core::Frame;

use crate::bgra_to_yuyv;

/// Find a v4l2loopback device under `<sysfs_root>/class/video4linux`.
/// Prefers one labelled `Page2Cam`, else any loopback device. Returns `/dev/<name>`.
pub fn find_loopback_device(sysfs_root: &Path) -> Option<PathBuf> {
    let base = sysfs_root.join("class/video4linux");
    let mut entries: Vec<_> = std::fs::read_dir(&base).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    let mut fallback = None;
    for e in entries {
        let name = std::fs::read_to_string(e.path().join("name")).unwrap_or_default();
        let driver = std::fs::read_link(e.path().join("device/driver"))
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let dev = PathBuf::from("/dev").join(e.file_name());
        if name.contains("Page2Cam") {
            return Some(dev);
        }
        let is_loopback = driver.contains("v4l2loopback")
            || name.contains("Dummy video device")
            || name.to_lowercase().contains("loopback");
        if is_loopback && fallback.is_none() {
            fallback = Some(dev);
        }
    }
    fallback
}

/// Explicit device wins; otherwise auto-detect a v4l2loopback device.
pub fn resolve_device(explicit: Option<&Path>, sysfs_root: &Path) -> anyhow::Result<PathBuf> {
    if let Some(d) = explicit {
        return Ok(d.to_path_buf());
    }
    find_loopback_device(sysfs_root).context(
        "no virtual camera device found. Load it with:\n  \
         sudo modprobe v4l2loopback exclusive_caps=1 card_label=Page2Cam",
    )
}

/// Writes frames to a v4l2loopback device as YUYV.
pub struct V4l2Sink {
    file: File,
    buf: Vec<u8>,
    spec: OutputSpec,
}

impl V4l2Sink {
    pub fn open(device: &Path, spec: &OutputSpec) -> anyhow::Result<V4l2Sink> {
        let file = OpenOptions::new()
            .write(true)
            .open(device)
            .with_context(|| format!("opening {}", device.display()))?;
        set_output_format(&file, spec.width, spec.height).with_context(|| {
            format!(
                "setting YUYV {}x{} on {}",
                spec.width,
                spec.height,
                device.display()
            )
        })?;
        Ok(V4l2Sink {
            file,
            buf: Vec::new(),
            spec: *spec,
        })
    }
}

impl FrameSink for V4l2Sink {
    fn push(&mut self, frame: &Frame) -> anyhow::Result<()> {
        anyhow::ensure!(
            frame.width == self.spec.width && frame.height == self.spec.height,
            "frame {}x{} does not match device format {}x{}",
            frame.width,
            frame.height,
            self.spec.width,
            self.spec.height
        );
        bgra_to_yuyv(frame, &mut self.buf);
        self.file.write_all(&self.buf)?;
        Ok(())
    }
}

// --- minimal V4L2 ABI (linux/videodev2.h), 64-bit layout ---
const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const V4L2_FIELD_NONE: u32 = 1;
const V4L2_COLORSPACE_SMPTE170M: u32 = 1;
const V4L2_QUANTIZATION_LIM_RANGE: u32 = 2;
const V4L2_PIX_FMT_YUYV: u32 = u32::from_le_bytes(*b"YUYV");

#[repr(C)]
#[derive(Clone, Copy)]
struct V4l2PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

#[repr(C, align(8))]
union V4l2FormatUnion {
    pix: V4l2PixFormat,
    raw: [u8; 200],
}

#[repr(C)]
struct V4l2Format {
    type_: u32,
    fmt: V4l2FormatUnion,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<V4l2Format>() == 208);

nix::ioctl_readwrite!(vidioc_s_fmt, b'V', 5, V4l2Format);

fn set_output_format(file: &File, width: u32, height: u32) -> anyhow::Result<()> {
    let mut fmt = V4l2Format {
        type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
        fmt: V4l2FormatUnion { raw: [0; 200] },
    };
    fmt.fmt.pix = V4l2PixFormat {
        width,
        height,
        pixelformat: V4L2_PIX_FMT_YUYV,
        field: V4L2_FIELD_NONE,
        bytesperline: width * 2,
        sizeimage: width * height * 2,
        colorspace: V4L2_COLORSPACE_SMPTE170M,
        priv_: 0,
        flags: 0,
        ycbcr_enc: 0,
        quantization: V4L2_QUANTIZATION_LIM_RANGE,
        xfer_func: 0,
    };
    // SAFETY: fmt is a correctly sized, initialized v4l2_format for this ioctl.
    unsafe { vidioc_s_fmt(file.as_raw_fd(), &mut fmt) }?;
    // SAFETY: the kernel filled the pix member for a VIDEO_OUTPUT type.
    let got = unsafe { fmt.fmt.pix };
    anyhow::ensure!(
        got.width == width && got.height == height && got.pixelformat == V4L2_PIX_FMT_YUYV,
        "device accepted {}x{} fourcc {:#x} instead of {width}x{height} YUYV",
        got.width,
        got.height,
        got.pixelformat
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fake_sysfs(devs: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("class/video4linux");
        fs::create_dir_all(&base).unwrap();
        for (dir, name) in devs {
            fs::create_dir_all(base.join(dir)).unwrap();
            fs::write(base.join(dir).join("name"), format!("{name}\n")).unwrap();
        }
        root
    }

    #[test]
    fn missing_device_error_mentions_modprobe() {
        let root = fake_sysfs(&[]);
        let err = resolve_device(None, root.path()).unwrap_err();
        assert!(
            format!("{err:#}").contains("modprobe v4l2loopback"),
            "{err:#}"
        );
    }

    #[test]
    fn explicit_device_wins() {
        let root = fake_sysfs(&[]);
        let got = resolve_device(Some(Path::new("/dev/video9")), root.path()).unwrap();
        assert_eq!(got, PathBuf::from("/dev/video9"));
    }

    #[test]
    fn find_device_returns_none_when_no_loopback() {
        let root = fake_sysfs(&[]);
        assert_eq!(find_loopback_device(root.path()), None);
        let root = fake_sysfs(&[("video0", "Integrated Camera")]);
        assert_eq!(find_loopback_device(root.path()), None);
    }

    #[test]
    fn find_device_prefers_page2cam_label() {
        let root = fake_sysfs(&[
            ("video0", "Integrated Camera"),
            ("video3", "Dummy video device (0x0000)"),
            ("video5", "Page2Cam"),
        ]);
        assert_eq!(
            find_loopback_device(root.path()),
            Some(PathBuf::from("/dev/video5"))
        );
    }

    #[test]
    fn find_device_falls_back_to_any_loopback() {
        let root = fake_sysfs(&[
            ("video0", "Integrated Camera"),
            ("video3", "Dummy video device (0x0000)"),
        ]);
        assert_eq!(
            find_loopback_device(root.path()),
            Some(PathBuf::from("/dev/video3"))
        );
    }
}
