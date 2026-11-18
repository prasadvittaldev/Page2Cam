# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Page2Cam is

A Rust desktop app that renders a live, interactive web page offscreen (embedded Chromium/CEF) and outputs it, with the user's real webcam as a movable picture-in-picture overlay, to:
- a **virtual camera** (Teams, Zoom, Meet...) and/or
- an **RTMP stream** (YouTube Live) with page and/or mic audio.

The operator drives it from a local **admin web page** (opened in the default browser): live preview, click/scroll/type into the page, PIP drag/resize, stream and audio controls.

The webcam can also be **cut out** (room removed, person over the page) with two ONNX models: Quality (Robust Video Matting) and Fast (MediaPipe selfie).

Design: `docs/superpowers/specs/2026-11-17-page2cam-design.md`, `docs/superpowers/specs/2026-11-18-webcam-cutout-design.md`. Plans: `docs/superpowers/plans/`.

Licence: **GPL-3.0-or-later** (because RVM is GPL-3.0; the user chose this). Third-party notices: `packaging/THIRD_PARTY.txt`.

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
./scripts/fetch-models.sh   # cut-out models + ONNX Runtime 1.24.4 (Linux .so, Windows DLLs) into third_party/, sha256-pinned
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
cargo test -p p2c-segment --test models -- --ignored --nocapture  # real models: shapes, cut-out of test-person.png, ms/frame
cargo run -p page2cam -- --fake-webcam third_party/models/test-person.png  # still image as the webcam (no camera needed)

# Windows
PATH=$HOME/.local/bin:$PATH CEF_PATH=$HOME/.local/share/cef-win cargo xwin build -p page2cam -p p2c-vcam-win-source --target x86_64-pc-windows-msvc
PATH=$HOME/.local/bin:$PATH cargo xwin clippy -p p2c-vcam-win-source -p p2c-vcam -p page2cam --target x86_64-pc-windows-msvc -- -D warnings
./scripts/package-windows.sh                              # -> dist/Page2Cam-windows-x64.zip, dist/Page2Cam-Setup-<version>.exe, dist/SHA256SUMS(.sig)
python3 scripts/make-brand-assets.py                      # regenerate icon/installer/admin images from assets/logo-source.png
```

## Architecture

Data flow: CEF off-screen page (BGRA frames → `FrameSlot`; page PCM → `AudioRing`) and webcam (→ `FrameSlot`; the segmenter copies it into a second "matte" `FrameSlot` with the person mask **in the alpha byte**, so frame and mask can't drift apart) → `Ticker` (fixed fps) composites → `FrameSink`s: virtual camera, `PreviewSink` (admin JPEG preview), `StreamSink` (ffmpeg). Mic + page audio → `Mixer` → ffmpeg.

Crates (`crates/`):
- `p2c-core`: `Frame` (packed BGRA8), `FrameSlot` (latest frame only + `seq` counter for staleness), `AudioRing` (bounded 48 kHz stereo f32 FIFO that drops the oldest). Hand-offs use slots/rings, never unbounded queues, so slow consumers can't build lag.
- `p2c-segment`: cut-out. ONNX Runtime is loaded **at run time** (`ort` `load-dynamic`, api-24) from `runtime_dir()` (exe dir if it holds the library, else `third_party/onnxruntime/<os>` **in debug builds only**; release builds look only next to the exe, so `target/release/page2cam` on Linux has no cut-out unless the files are copied next to it), so nothing links at build time and the xwin cross-build is unaffected. `RvmEngine` (640×360 in, recurrent state carried and reset on size/engine change, DirectML preferred on Windows) and `FastEngine` (256×144, CPU). `Segmenter` thread: disabled unless a cut-out style is on, processes each webcam frame once, `FallbackPolicy` switches Quality→Fast when inference is slower than `max(camera interval, 1/15 s)` for 3 s (manual switch re-arms; auto switch never saved). RVM state is reset on size change, re-enable and webcam stalls >1 s; 10 failed inferences in a row reopen the engine. Model input is scaled with `scale_frame_straight` (alpha is data, not transparency).
- `p2c-compositor`: `compose()` (letterbox page + PIP) and `Ticker` (absolute-deadline pacing; always emits frames, black without a page; a failing sink is logged and doesn't block others). `PipLayout { visible, x, y, width, style }`: x/y are 0..1 positions inside a 2% margin (corners exactly 0/1), width is a fraction of the output width, height follows the webcam aspect. `OverlayStyle`: Box (opaque, ignores alpha), CutoutSmall (side margins, stands on the bottom edge, y ignored), CutoutFull (full height, bottom, x slides, width ignored); cut-outs alpha-blend the matte. A cut-out style without a fresh matte draws the raw webcam as a Box (never a frozen cut-out). Output frames are always opaque. It lives in `Arc<RwLock<_>>` and the admin page changes it live. The admin UI JS mirrors this geometry exactly. The PIP hides when the webcam slot hasn't changed for 1 s.
- `p2c-browser`: CEF. **CEF owns the main thread**: `main` must call `maybe_run_subprocess()` first (CEF re-launches the exe as helpers), then `run()` pumps `do_message_loop_work` on the main thread. Other threads use the `Send` `BrowserHandle` (commands over a channel): navigate, back/forward/reload, mouse/wheel/key input, `current_url()`. Device scale is fixed at 1.0. `on_paint` copies the full buffer. With an `AudioRing` set, page audio goes to the ring (and not to the speakers). CHAR key events carry the character in `windows_key_code` on Windows but the VK code on Linux (`key_code_for`).
- `p2c-webcam`: nokhwa capture with **`default-features = false`** (its mozjpeg C dep won't cross-compile). MJPEG/YUYV/NV12 are decoded in pure Rust (`decode.rs`). `Camera` is `!Send`, so it's opened on the capture thread. `pick_camera` never returns our own output device (by canonical path or the "Page2Cam" name), which would create a feedback loop.
- `p2c-audio`: `Mixer` (per-source on/gain, clamp, silence fill), `Resampler` (linear), `MicCapture` (cpal; started only when the operator enables the mic).
- `p2c-stream`: `StreamController` runs ffmpeg: raw BGRA on stdin plus f32le audio over a localhost TCP socket that **we** listen on and ffmpeg connects to. Inputs use `-probesize 32 -analyzeduration 0`, otherwise ffmpeg stalls ~5 s probing real-time audio. Video/audio writers are paced threads. Status (connecting/live/error) is parsed from ffmpeg's stderr (`frame=` = live). `redact()` keeps the stream key out of logs.
- `p2c-admin`: axum on `127.0.0.1` (port 8765 or any free port), its own tokio runtime thread. Every request needs the per-launch token (`X-P2C-Token` header or `?t=`) **and** a `Host` of `127.0.0.1:<port>`/`localhost:<port>` (DNS-rebinding guard). It talks to the app only through the `Controls` trait (tests use a fake, so it doesn't link CEF). UI is a single file `ui/index.html` (no external resources). WebSocket `/ws`: binary JPEG preview out, JSON input in (0..1 preview coords → page px via `map_preview`). `normalize_url` refuses non-http(s)/file schemes.
- `p2c-vcam`: `open_default()` platform seam. Linux: `V4l2Sink` (YUYV via a hand-written `VIDIOC_S_FMT` ioctl; no bindgen/libclang). Windows: `windows_cam.rs` checks that the CLSID is registered in HKLM, runs `MFCreateVirtualCamera(...).Start()` on an MTA thread, and writes frames into the DLL's shared memory. The camera is optional at runtime: failure → status shows the error, app keeps running.
- `p2c-shm`: shared-memory protocol (`Global\Page2CamFrames`, seqlocked header + BGRA ≤1920×1080), BGRA→NV12/RGB32 with pitch and scaling, and the camera CLSID. Platform-free, unit-tested.
- `p2c-vcam-win-source`: the COM media-source DLL loaded by Windows' Frame Server service, **ported from smourier/VCamSample (MIT)**: IMFActivate → IMFMediaSourceEx + IMFMediaStream2, IMFAttributes forwarded via vtable macro (`attrs.rs`). It ignores the D3D manager (CPU samples), offers NV12/RGB32 at 1080p/720p, and shows a "waiting" pattern when frames are >2 s old. **The DLL creates the Global mapping** (a normal user process lacks the privilege; the app opens it, retrying each second). Logs to `C:\ProgramData\Page2Cam\vcam.log`. Must be registered from a folder the service can read (`install-camera.cmd` copies it to Program Files and runs regsvr32).
- `p2c-update`: update checks against GitHub Releases (`fetch_latest` via the system `curl`, which ships with Windows 10+), `parse_release` (needs `Page2Cam-Setup-<v>.exe`, `SHA256SUMS`, `SHA256SUMS.sig`), `verify_sums` (ed25519, public key `RELEASE_PUBLIC_KEY`), `download_verified`. The private release key lives only at `~/.config/page2cam/release-signing.key` (back it up; losing it means installed copies can no longer verify updates). `examples/release_key.rs` creates/signs.
- `page2cam`: binary. `update.rs`: `Updater` checks 15 s after start and every 6 h (`check_updates` config), shows a banner in the admin page; installed copies (Uninstall.exe next to the exe) download+verify the setup and start it elevated with `/S /RELAUNCH`, then close. `overlay_style`/`cutout_engine` chosen in the admin page are written back to the settings file with `toml_edit` (comments kept). Mic is delayed by the cut-out's inference latency (median of ~2 s, capped at 150 ms; `Mixer::set_mic_delay_ms` keeps that much mic audio queued, rebuilt after resets/underruns) so lips match on the stream. `onnxruntime.dll` needs the VC++ runtime: the zip ships `vc_redist.x64.exe` and `install-camera.cmd` installs it. Settings: `--config`, else `page2cam.toml` next to the exe, else `<config_dir>/page2cam/config.toml`; CLI overrides. Log: `page2cam.log` next to the exe on Windows, data dir on Linux. The Windows manifest is compiled with `llvm-rc` in `build.rs` (lld can't embed manifests without `mt.exe`). `App` implements `Controls`. The stream key is optionally stored in the OS keyring (`keyring` v3) and never logged.

CEF test binaries use `harness = false` (libtest rejects CEF's helper-process flags), and CEF can initialize only once per process, so all render checks run as steps of one session.

## Installer

`packaging/windows/installer.nsi` (NSIS 3, amd64, per-machine, admin). NSIS runs on Linux from an unpacked Ubuntu package at `~/.local/share/nsis-pkg/root` (`apt-get download nsis nsis-common` + `dpkg -x`; override with `NSIS_HOME`); the build uses `-WX`. The installer stops page2cam and the FrameServer services, moves a locked old camera DLL aside (deleted on reboot), installs vc_redist, regsvr32s the DLL, adds shortcuts and an Apps entry, and with `/RELAUNCH` restarts Page2Cam as the user (via explorer.exe). Installed copies keep settings in `%APPDATA%\page2cam` (no page2cam.toml next to the exe) and logs in the user data dir. The zip stays as the portable option.

Branding: `assets/logo-source.png` (transparent) → `scripts/make-brand-assets.py` → exe icon (`crates/page2cam/windows/page2cam.ico`, embedded via page2cam.rc), installer images, admin `ui/logo.png`/`ui/icon.png` (served at `/ui/*.png`, token required). Admin theme colours: navy `#0d1b2a`, brand blue `#0b63f0`, white.

## Known open risks

- The Windows camera DLL **works on the user's PC** (seen in Teams/Meet after running install-camera.cmd). Teams remote viewing (others in the call seeing it) is not yet confirmed.
- DirectML (GPU) for the Quality cut-out is compile-checked only. On this dev box (Ryzen 5 8600G, CPU, release) Quality takes ~31 ms/frame (20–23 masks/s end to end), Fast ~1 ms.
- The webcam-mode choice (`choose_format`, aiming at ~30 fps) has never run against a real camera; the user's integrated webcam was reported as "1280x720@1".
- Linux v4l2loopback writing and webcam capture have never run against real devices on the dev box (no module loaded, no webcam).
