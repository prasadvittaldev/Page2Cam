# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Page2Cam is

A Rust desktop app that renders a live, interactive web page offscreen and outputs it, with the user's real webcam as a movable picture-in-picture overlay, to:
- a **virtual camera** (picked up by Teams, Zoom, Meet and similar apps), and/or
- an **RTMP stream** (YouTube Live), with page and/or mic audio.

The full design is in `docs/superpowers/specs/2026-11-17-page2cam-design.md`. Implementation plans are in `docs/superpowers/plans/`. Milestone 1 (page + webcam PIP → Linux virtual camera) is the current scope; the admin UI, audio/RTMP, the Windows camera and packaging come later.

## Hard constraints (decided by the user)

- Targets **Windows 11 and Linux only**. No macOS, no Windows 10.
- **No OBS dependency.** The app provides its own virtual camera (Linux: `v4l2loopback`; Windows 11: `MFCreateVirtualCamera` with a COM media source DLL fed through shared memory).
- Page audio goes to the stream only, not into call apps.

## Setup (Linux)

```sh
# Rust via rustup; CEF binaries (~1.5 GB, version must match the pinned `cef` crate =156.6.0)
cargo install export-cef-dir --version =156.6.0+156.0.30
export-cef-dir --force $HOME/.local/share/cef
export CEF_PATH=$HOME/.local/share/cef     # needed for every cargo build/test/run

# Virtual camera device (once per boot)
sudo modprobe v4l2loopback exclusive_caps=1 card_label=Page2Cam
```

Build scripts in `page2cam` and `p2c-browser` embed an rpath to `$CEF_PATH`, so `LD_LIBRARY_PATH` isn't needed in dev.

## Commands

```sh
cargo build
cargo test --workspace                                  # unit tests; hardware/CEF tests are skipped
cargo test -p p2c-compositor tick_scales_mismatched_page # single test
cargo test -p p2c-browser --test render -- --ignored     # CEF render tests (needs CEF_PATH)
cargo test -p p2c-vcam -- --ignored                      # writes to the real v4l2loopback device
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt

cargo run -p page2cam -- --test-pattern                  # moving bar → camera, no browser
cargo run -p page2cam -- --url https://example.com       # web page → camera (+ webcam PIP bottom-right)
cargo run -p page2cam -- --url https://example.com --pip top-left --pip-size 20 --webcam c920
cargo run -p page2cam -- --url https://example.com --pip off
cargo run -p p2c-webcam --example list                   # cameras the PIP picker can see
ffplay -f v4l2 /dev/videoN                               # watch the camera output
```

## Architecture

Crates in `crates/`:
- `p2c-core`: `Frame` (tightly packed BGRA8) and `FrameSlot` (holds only the latest frame, plus a `seq` counter for staleness). All producer/consumer hand-off goes through slots, not queues, so a slow consumer never builds lag.
- `p2c-compositor`: `compose()` plus `Ticker`, a thread that composites at a fixed fps using absolute deadlines and pushes to `FrameSink`s. It always emits frames (black when there's no page) so call apps never see a frozen camera. A failing sink is logged and doesn't block the others.
- `p2c-vcam`: BGRA→YUYV (BT.601 limited) and `V4l2Sink`. It sets the format with a hand-written `VIDIOC_S_FMT` ioctl (via `nix`), not a bindgen crate, so no libclang is needed.
- `p2c-browser`: CEF off-screen rendering. **CEF owns the main thread**: `main` must call `maybe_run_subprocess()` first (CEF re-launches the same exe as helper processes), then `run()` blocks the main thread pumping `do_message_loop_work`. Other threads control it via the `Send` `BrowserHandle`, which sends commands over a channel. `on_paint` always copies the full view buffer. The device scale factor is fixed at 1.0, so frames are exactly the output size.
- `p2c-webcam`: nokhwa capture into its own `FrameSlot`. The nokhwa `Camera` is `!Send`, so it is opened and read on the capture thread. `pick_camera` never returns Page2Cam's own output device (by path or the `Page2Cam` name), which would create a feedback loop.
- PIP: `PipLayout { visible, x, y, width }` lives behind `Arc<RwLock<_>>` so it can be changed live (the admin UI will). `x`/`y` are 0..1 positions within the area inside a 2% margin (corners are exactly 0/1); `width` is a fraction of the output width and height follows the webcam aspect. `pip_rect` always clamps inside the frame. The ticker hides the PIP when the webcam slot's `seq` hasn't changed for 1 s.
- `page2cam`: binary; TOML config (`<config_dir>/page2cam/config.toml`) with CLI overrides; wires browser → slot → ticker → v4l2 sink.

CEF test binaries use `harness = false` (libtest would reject CEF's helper-process flags), and CEF can initialize only once per process, so the render checks share one run.

Security invariants for later milestones: the admin server binds to `127.0.0.1` only; every admin request needs the per-launch random token; the stream key lives in the OS keyring and never appears in logs or the UI.
