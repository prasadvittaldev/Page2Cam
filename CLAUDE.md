# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Page2Cam is

A Rust desktop app that renders a live, interactive web page offscreen (embedded Chromium/CEF) and outputs it, with the user's real webcam as a movable picture-in-picture overlay, to:
- a **virtual camera** (Teams, Zoom, Meet...) and/or
- an **RTMP stream** (YouTube Live) with page and/or mic audio.

The operator drives it from a local **admin web page** (opened in the default browser): live preview, click/scroll/type into the page, PIP drag/resize, stream and audio controls.

Design: `docs/superpowers/specs/2026-11-17-page2cam-design.md`. Plans: `docs/superpowers/plans/`.

## Hard constraints (decided by the user)

- **Windows 11 and Linux only.** No macOS, no Windows 10.
- **No OBS dependency.** Own virtual camera: Linux `v4l2loopback`; Windows 11 `MFCreateVirtualCamera` + our COM media-source DLL.
- Page audio goes to the stream only, not into call apps.
- The user tests Windows builds on their own PC; Windows-specific code can only be compile-checked from this Linux box, so make failures self-explaining (logs, admin status).

## Setup (Linux dev box)

```sh
cargo install export-cef-dir --version =156.6.0+156.0.30   # must match the pinned `cef` crate
export-cef-dir --force $HOME/.local/share/cef                  # Linux CEF (~1.5 GB)
export CEF_PATH=$HOME/.local/share/cef                         # needed for every cargo build/test/run
sudo modprobe v4l2loopback exclusive_caps=1 card_label=Page2Cam  # virtual camera device, once per boot
```

Windows cross-build (no Windows machine needed): `rustup target add x86_64-pc-windows-msvc`, `cargo install cargo-xwin`, `export-cef-dir --force --target x86_64-pc-windows-msvc $HOME/.local/share/cef-win`, plus `clang-cl` and `lld-link` on PATH (here they're shims in `~/.local/bin`: `clang-cl` → llvm-19 `clang`, `lld-link` → rustup's `rust-lld -flavor link`). **Use `CEF_PATH=$HOME/.local/share/cef-win` for Windows builds**; the Linux CEF dir fails with "cef_sandbox_win.h not found".

## Commands

```sh
cargo build
cargo test --workspace                                   # unit tests; hardware/CEF/network tests are ignored
cargo test -p p2c-compositor tick_scales_mismatched_page  # single test
cargo test -p p2c-browser --test render -- --ignored      # real CEF: render, partial repaint, click, keys, audio, URL
cargo test -p p2c-stream --test rtmp_local -- --ignored   # streams to a local ffmpeg RTMP listener, ffprobes the result
cargo test -p p2c-vcam -- --ignored                       # writes to the real v4l2loopback device
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt

cargo run -p page2cam -- --url https://example.com        # app + admin page (token URL printed on stdout)
cargo run -p page2cam -- --test-pattern --no-open         # moving bar instead of a page
cargo run -p p2c-webcam --example list                    # cameras the PIP picker can see

# Windows
PATH=$HOME/.local/bin:$PATH CEF_PATH=$HOME/.local/share/cef-win cargo xwin build -p page2cam -p p2c-vcam-win-source --target x86_64-pc-windows-msvc
PATH=$HOME/.local/bin:$PATH cargo xwin clippy -p p2c-vcam-win-source -p p2c-vcam -p page2cam --target x86_64-pc-windows-msvc -- -D warnings
./scripts/package-windows.sh                              # -> dist/Page2Cam-windows-x64.zip (release, CEF runtime, ffmpeg.exe, install scripts)
```

## Architecture

Data flow: CEF off-screen page (BGRA frames → `FrameSlot`; page PCM → `AudioRing`) and webcam (→ `FrameSlot`) → `Ticker` (fixed fps) composites → `FrameSink`s: virtual camera, `PreviewSink` (admin JPEG preview), `StreamSink` (ffmpeg). Mic + page audio → `Mixer` → ffmpeg.

Crates (`crates/`):
- `p2c-core`: `Frame` (packed BGRA8), `FrameSlot` (latest frame only + `seq` counter for staleness), `AudioRing` (bounded 48 kHz stereo f32 FIFO that drops the oldest). Hand-offs use slots/rings, never unbounded queues, so slow consumers can't build lag.
- `p2c-compositor`: `compose()` (letterbox page + PIP) and `Ticker` (absolute-deadline pacing; always emits frames, black without a page; a failing sink is logged and doesn't block others). `PipLayout { visible, x, y, width }`: x/y are 0..1 positions inside a 2% margin (corners exactly 0/1), width is a fraction of the output width, height follows the webcam aspect. It lives in `Arc<RwLock<_>>` and the admin page changes it live. The admin UI JS mirrors this geometry exactly. The PIP hides when the webcam slot hasn't changed for 1 s.
- `p2c-browser`: CEF. **CEF owns the main thread**: `main` must call `maybe_run_subprocess()` first (CEF re-launches the exe as helpers), then `run()` pumps `do_message_loop_work` on the main thread. Other threads use the `Send` `BrowserHandle` (commands over a channel): navigate, back/forward/reload, mouse/wheel/key input, `current_url()`. Device scale is fixed at 1.0. `on_paint` copies the full buffer. With an `AudioRing` set, page audio goes to the ring (and not to the speakers). CHAR key events carry the character in `windows_key_code` on Windows but the VK code on Linux (`key_code_for`).
- `p2c-webcam`: nokhwa capture with **`default-features = false`** (its mozjpeg C dep won't cross-compile). MJPEG/YUYV/NV12 are decoded in pure Rust (`decode.rs`). `Camera` is `!Send`, so it's opened on the capture thread. `pick_camera` never returns our own output device (by canonical path or the "Page2Cam" name), which would create a feedback loop.
- `p2c-audio`: `Mixer` (per-source on/gain, clamp, silence fill), `Resampler` (linear), `MicCapture` (cpal; started only when the operator enables the mic).
- `p2c-stream`: `StreamController` runs ffmpeg: raw BGRA on stdin plus f32le audio over a localhost TCP socket that **we** listen on and ffmpeg connects to. Inputs use `-probesize 32 -analyzeduration 0`, otherwise ffmpeg stalls ~5 s probing real-time audio. Video/audio writers are paced threads. Status (connecting/live/error) is parsed from ffmpeg's stderr (`frame=` = live). `redact()` keeps the stream key out of logs.
- `p2c-admin`: axum on `127.0.0.1` (port 8765 or any free port), its own tokio runtime thread. Every request needs the per-launch token (`X-P2C-Token` header or `?t=`) **and** a `Host` of `127.0.0.1:<port>`/`localhost:<port>` (DNS-rebinding guard). It talks to the app only through the `Controls` trait (tests use a fake, so it doesn't link CEF). UI is a single file `ui/index.html` (no external resources). WebSocket `/ws`: binary JPEG preview out, JSON input in (0..1 preview coords → page px via `map_preview`). `normalize_url` refuses non-http(s)/file schemes.
- `p2c-vcam`: `open_default()` platform seam. Linux: `V4l2Sink` (YUYV via a hand-written `VIDIOC_S_FMT` ioctl; no bindgen/libclang). Windows: `windows_cam.rs` checks that the CLSID is registered in HKLM, runs `MFCreateVirtualCamera(...).Start()` on an MTA thread, and writes frames into the DLL's shared memory. The camera is optional at runtime: failure → status shows the error, app keeps running.
- `p2c-shm`: shared-memory protocol (`Global\Page2CamFrames`, seqlocked header + BGRA ≤1920×1080), BGRA→NV12/RGB32 with pitch and scaling, and the camera CLSID. Platform-free, unit-tested.
- `p2c-vcam-win-source`: the COM media-source DLL loaded by Windows' Frame Server service, **ported from smourier/VCamSample (MIT)**: IMFActivate → IMFMediaSourceEx + IMFMediaStream2, IMFAttributes forwarded via vtable macro (`attrs.rs`). It ignores the D3D manager (CPU samples), offers NV12/RGB32 at 1080p/720p, and shows a "waiting" pattern when frames are >2 s old. **The DLL creates the Global mapping** (a normal user process lacks the privilege; the app opens it, retrying each second). Logs to `C:\ProgramData\Page2Cam\vcam.log`. Must be registered from a folder the service can read (`install-camera.cmd` copies it to Program Files and runs regsvr32).
- `page2cam`: binary. Settings: `--config`, else `page2cam.toml` next to the exe, else `<config_dir>/page2cam/config.toml`; CLI overrides. Log: `page2cam.log` next to the exe on Windows, data dir on Linux. The Windows manifest is compiled with `llvm-rc` in `build.rs` (lld can't embed manifests without `mt.exe`). `App` implements `Controls`. The stream key is optionally stored in the OS keyring (`keyring` v3) and never logged.

CEF test binaries use `harness = false` (libtest rejects CEF's helper-process flags), and CEF can initialize only once per process, so all render checks run as steps of one session.

## Known open risks

- The Windows camera DLL has only been compile-checked, never run. VCamSample's README reports Teams showing the camera in the local preview but not to remote participants. Treat Teams remote viewing as unverified.
- Linux v4l2loopback writing and webcam capture have never run against real devices on the dev box (no module loaded, no webcam).
