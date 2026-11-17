# Milestone 1: Web Page → Linux Virtual Camera — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `page2cam --url https://example.com --pip bottom-right` renders the page offscreen with embedded Chromium, overlays the user's real webcam as a picture-in-picture, and publishes the result at a steady 1920×1080@30 as a v4l2loopback webcam that Teams, Zoom and browsers can select.

**Architecture:** A Cargo workspace. `p2c-core` holds the shared frame types and a "latest frame" slot. `p2c-compositor` runs a fixed-rate ticker that reads the newest page frame, fits it to the output size and hands the result to sinks. `p2c-vcam` converts BGRA to YUYV and writes to `/dev/videoN`. `p2c-browser` wraps CEF off-screen rendering and writes frames into a slot. The `page2cam` binary wires these together. CEF owns the main thread; the compositor runs on its own thread.

**Tech Stack:** Rust stable (2021 edition), `cef` (tauri-apps/cef-rs, pinned), `v4l`, `fast_image_resize`, `clap`, `serde` + `toml`, `directories`, `anyhow`/`thiserror`, `tracing`.

**Spec:** `docs/superpowers/specs/2026-11-17-page2cam-design.md`

**Scope:** This plan covers spec milestones 1 and 2 (page → camera, plus webcam PIP). Milestones 3–6 (admin UI, audio + RTMP, Windows 11 vcam, packaging) each get their own plan after this one ships, and build on the interfaces defined here.

## Global Constraints

- Target platforms: Windows 11 and Linux only. This milestone is Linux-only, but nothing in `p2c-core`/`p2c-compositor` may use Linux-specific APIs.
- No OBS dependency.
- Default output: 1920×1080 @ 30 fps; 1280×720 must also work.
- Output frame rate stays steady even when the page produces no new frames.
- Internal pixel format is BGRA8 (CEF's native format), tightly packed (`stride == width * 4`).

## Review Focus

1. **v4l2loopback not loaded / no device:** expect a clear error naming the fix (`sudo modprobe v4l2loopback exclusive_caps=1 card_label=Page2Cam`), not a panic. Test in Task 4 (`find_device_returns_none_when_no_loopback`) and Task 7 (`missing_device_error_mentions_modprobe`).
2. **Page never loads (bad URL, offline):** the camera must keep emitting frames (black) at the configured fps, otherwise call apps show "camera frozen". Test in Task 3 (`tick_without_page_emits_black`).
3. **Page frame size ≠ output size** (HiDPI scale factor, resize mid-run): scale to fit, never write a wrong-sized buffer. Test in Task 3 (`tick_scales_mismatched_page`).
4. **Odd width/height in config:** YUYV needs an even width, so config validation must reject it. Test in Task 5 (`rejects_odd_dimensions`).
5. **CEF partial repaints (dirty rects):** the slot must always receive the *full* view buffer, never just a dirty sub-rect. Verified in Task 6's integration test by changing only part of the page and checking an unchanged region keeps its color.

Also covered (PIP):
- **Feedback loop:** the webcam picker must never pick Page2Cam's own loopback device, otherwise the output contains itself. Test in Task 8 (`picker_skips_own_loopback`).
- **Webcam unplugged or stalled mid-run:** the PIP disappears after 1 s without a new webcam frame instead of showing a frozen face. Test in Task 9 (`stale_pip_is_hidden`).
- **PIP placed partly off-frame:** clamp it inside the output instead of panicking on out-of-bounds writes. Test in Task 9 (`pip_clamped_inside_frame`).

---

### Task 1: Toolchain, workspace, and `p2c-core` frame types

**Files:**
- Create: `Cargo.toml` (workspace), `rust-toolchain.toml` (`channel = "stable"`), `.gitignore` (`/target`, `/third_party/cef`)
- Create: `crates/p2c-core/Cargo.toml`, `crates/p2c-core/src/lib.rs`, `crates/p2c-core/src/frame.rs`, `crates/p2c-core/src/slot.rs`

**Interfaces:**
- Produces:
  - `p2c_core::Frame { pub width: u32, pub height: u32, pub data: Vec<u8> }` (BGRA8, len = w*h*4)
  - `Frame::new_black(width: u32, height: u32) -> Frame` (B=G=R=0, A=255)
  - `Frame::from_bgra(width: u32, height: u32, data: Vec<u8>) -> Result<Frame, FrameError>` (errors if len ≠ w*h*4)
  - `Frame::pixel(&self, x: u32, y: u32) -> [u8; 4]`
  - `p2c_core::FrameSlot` (Clone, Send + Sync): `new() -> Self`, `put(&self, f: Frame)`, `latest(&self) -> Option<Arc<Frame>>`, `seq(&self) -> u64` (increments on each put)

- [ ] **Step 1: Install prerequisites.** `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y` then `sudo apt install -y build-essential pkg-config libclang-dev v4l-utils cmake ninja-build`. Verify: `cargo --version` prints a version.
- [ ] **Step 2: `git init`**, create the workspace files and an empty `p2c-core` crate. `cargo build` succeeds.
- [ ] **Step 3: Write failing tests** in `frame.rs`/`slot.rs`:
  - `new_black_is_opaque_black`: `Frame::new_black(4,2).pixel(3,1) == [0,0,0,255]`, `data.len() == 32`
  - `from_bgra_rejects_wrong_len`: `Frame::from_bgra(2,2,vec![0;15]).is_err()`
  - `slot_empty_returns_none`: `FrameSlot::new().latest().is_none()`
  - `slot_returns_latest_and_counts`: put A then B → `latest()` is B (check width), `seq() == 2`
  - `slot_shared_across_threads`: clone into a spawned thread, put there, join, `latest().is_some()`
- [ ] **Step 4: Run** `cargo test -p p2c-core`. Expected: FAIL (items undefined).
- [ ] **Step 5: Implement.** `FrameSlot` = `Arc<Mutex<(Option<Arc<Frame>>, u64)>>` (`parking_lot` is fine).
- [ ] **Step 6: Run** `cargo test -p p2c-core`. Expected: PASS. Also `cargo clippy --workspace -- -D warnings` is clean.
- [ ] **Step 7: Commit** `feat(core): frame type and latest-frame slot`.

### Task 2: BGRA → YUYV conversion

**Files:**
- Create: `crates/p2c-vcam/Cargo.toml`, `crates/p2c-vcam/src/lib.rs`, `crates/p2c-vcam/src/convert.rs`

**Interfaces:**
- Consumes: `p2c_core::Frame`
- Produces: `p2c_vcam::bgra_to_yuyv(src: &Frame, dst: &mut Vec<u8>)`. Resizes `dst` to `w*h*2`; BT.601 limited range; U/V averaged over each horizontal pixel pair. Requires even width (`debug_assert`).

- [ ] **Step 1: Write failing tests** (2×1 frames of a single color; check `dst == [Y,U,Y,V]`, tolerance ±1):
  - `white`: BGRA(255,255,255,255) → Y=235, U=128, V=128
  - `black`: → Y=16, U=128, V=128
  - `red`: BGRA(0,0,255,255) → Y=81, U=90, V=240
  - `output_len`: 4×2 frame → `dst.len() == 16`
- [ ] **Step 2: Run** `cargo test -p p2c-vcam convert`. Expected: FAIL.
- [ ] **Step 3: Implement** using integer math: `Y = ((66R + 129G + 25B + 128) >> 8) + 16`, `U = ((-38R - 74G + 112B + 128) >> 8) + 128`, `V = ((112R - 94G - 18B + 128) >> 8) + 128`.
- [ ] **Step 4: Run** the tests. Expected: PASS.
- [ ] **Step 5: Commit** `feat(vcam): BGRA to YUYV conversion`.

### Task 3: Compositor with fixed-rate ticker

**Files:**
- Create: `crates/p2c-compositor/Cargo.toml`, `src/lib.rs`, `src/compose.rs`, `src/ticker.rs`

**Interfaces:**
- Consumes: `Frame`, `FrameSlot`
- Produces:
  - `p2c_compositor::OutputSpec { pub width: u32, pub height: u32, pub fps: u32 }`
  - `trait FrameSink: Send { fn push(&mut self, frame: &Frame) -> anyhow::Result<()>; }`
  - `fn compose(page: Option<&Frame>, out: &mut Frame)`: black if `None`; copy if sizes match; otherwise scale to fit with letterbox (aspect preserved, centered, black bars) using `fast_image_resize`. Milestone 2 adds a PIP parameter.
  - `struct Ticker`: `Ticker::spawn(spec: OutputSpec, page: FrameSlot, sinks: Vec<Box<dyn FrameSink>>) -> Ticker` (runs on its own `std::thread`), `Ticker::stop(self)` (joins). Per tick: compose, then push to each sink. A failing sink is logged (`tracing::warn!`, rate-limited to once per second) and keeps getting frames, so the others are unaffected. Pace with absolute deadlines (`start + n * period`), not by sleeping one period each tick, so timing doesn't drift.

- [ ] **Step 1: Write failing tests:**
  - `tick_without_page_emits_black`: `compose(None, &mut out)` → every sampled pixel `[0,0,0,255]`
  - `compose_same_size_copies`: 4×4 red page → out pixel (2,2) red
  - `tick_scales_mismatched_page`: 2×2 red page, out 8×4 → out dims unchanged 8×4; center (4,2) red; (0,2) black (letterbox)
  - `ticker_runs_at_fps`: a test sink that counts pushes; `OutputSpec{w:16,h:16,fps:30}`, run 1 s, stop → count in 27..=33
  - `failing_sink_does_not_starve_others`: one sink always `Err`, one counting → counting sink still ≥ 27 in 1 s
- [ ] **Step 2: Run** `cargo test -p p2c-compositor`. Expected: FAIL.
- [ ] **Step 3: Implement** `compose` and `Ticker` per the interface. Reuse the output `Frame` buffer between ticks.
- [ ] **Step 4: Run** the tests. Expected: PASS.
- [ ] **Step 5: Commit** `feat(compositor): compose with letterbox and fixed-rate ticker`.

### Task 4: v4l2loopback sink

**Files:**
- Create: `crates/p2c-vcam/src/linux.rs` (`#[cfg(target_os = "linux")]`)
- Test: inline unit tests, and `crates/p2c-vcam/tests/v4l2_device.rs` (`#[ignore]`)

**Interfaces:**
- Consumes: `bgra_to_yuyv`, `FrameSink`, `OutputSpec` (add `p2c-compositor` as a dependency)
- Produces:
  - `p2c_vcam::find_loopback_device(sysfs_root: &Path) -> Option<PathBuf>`: scan `<root>/class/video4linux/*/name`; prefer one whose name contains `Page2Cam`, else any whose `<dir>/device/driver` link or name mentions `v4l2 loopback`/`Dummy video device`; return `/dev/<dirname>`. Production passes `Path::new("/sys")`.
  - `p2c_vcam::V4l2Sink::open(device: &Path, spec: &OutputSpec) -> anyhow::Result<V4l2Sink>`: set output format YUYV w×h through the `v4l` crate (`v4l::video::Output::set_format`), then write frames with plain `write()` on the opened fd. Implements `FrameSink`.

- [ ] **Step 1: Write failing tests** using a temp dir that mimics sysfs (`tempfile`):
  - `find_device_returns_none_when_no_loopback`: empty `class/video4linux` → `None`
  - `find_device_prefers_page2cam_label`: `video0/name = "Integrated Camera"`, `video5/name = "Page2Cam"` → `/dev/video5`
  - `tests/v4l2_device.rs::writes_frames` (`#[ignore]`): find device under `/sys`, open at 1280×720, push 30 frames, no error
- [ ] **Step 2: Run** `cargo test -p p2c-vcam`. Expected: FAIL.
- [ ] **Step 3: Implement** per the interface.
- [ ] **Step 4: Run** unit tests (PASS). Then `sudo modprobe v4l2loopback exclusive_caps=1 card_label=Page2Cam` and `cargo test -p p2c-vcam -- --ignored`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(vcam): v4l2loopback sink and device discovery`.

### Task 5: Config and `page2cam --test-pattern`

**Files:**
- Create: `crates/page2cam/Cargo.toml`, `src/main.rs`, `src/config.rs`, `src/pattern.rs`

**Interfaces:**
- Consumes: `FrameSlot`, `Ticker`, `OutputSpec`, `V4l2Sink`, `find_loopback_device`
- Produces:
  - `Config { url: String, width: u32, height: u32, fps: u32, vcam_device: Option<PathBuf> }` with serde defaults `url = "https://example.com"`, `1920`, `1080`, `30`, `None`
  - `Config::load(path: Option<&Path>) -> anyhow::Result<Config>`: explicit path, else `<config_dir>/page2cam/config.toml` from `directories`, else defaults. Then `validate()`.
  - `Config::validate(&self) -> anyhow::Result<()>`: width/height even and in 320..=3840 / 240..=2160; fps 1..=60
  - CLI (`clap`): `--config <path>`, `--url <url>`, `--width`, `--height`, `--fps`, `--device <path>`, `--test-pattern`. CLI flags override the file.
  - `pattern::spawn(slot: FrameSlot, w: u32, h: u32)`: a thread that writes a frame every 33 ms with a vertical white bar moving across a dark-gray background

- [ ] **Step 1: Write failing tests** in `config.rs`:
  - `defaults_are_1080p30`: `Config::default()` → 1920/1080/30
  - `partial_toml_fills_defaults`: `toml::from_str("fps = 60")` → fps 60, width 1920
  - `rejects_odd_dimensions`: width 1281 → `validate().is_err()`
  - `rejects_fps_zero`: fps 0 → err
- [ ] **Step 2: Run** `cargo test -p page2cam`. Expected: FAIL.
- [ ] **Step 3: Implement** config, CLI and pattern, plus a `main` that, with `--test-pattern`, wires pattern → slot → Ticker → V4l2Sink and runs until Ctrl-C (`ctrlc` crate).
- [ ] **Step 4: Run** the tests (PASS). Then a manual check: `cargo run -p page2cam -- --test-pattern --width 1280 --height 720` and, in another terminal, `ffplay -f v4l2 /dev/videoN` shows the moving bar smoothly. Also open https://webcamtests.com in Chrome, pick "Page2Cam" and confirm the bar appears.
- [ ] **Step 5: Commit** `feat: config, CLI, and test-pattern to virtual camera`.

### Task 6: `p2c-browser`: CEF off-screen rendering into a FrameSlot

**Files:**
- Create: `crates/p2c-browser/Cargo.toml`, `src/lib.rs`, `src/app.rs` (CEF App/BrowserProcessHandler), `src/client.rs` (Client + RenderHandler + LoadHandler)
- Create: `crates/p2c-browser/tests/render.rs` (`#[ignore]`)
- Modify: `.gitignore` (CEF download dir)

**Interfaces:**
- Consumes: `Frame`, `FrameSlot`
- Produces:
  - `p2c_browser::maybe_run_subprocess() -> Option<i32>`: must be the first call in `main`. Returns `Some(exit_code)` when this process is a CEF helper subprocess (the caller exits with it), `None` in the browser process.
  - `p2c_browser::BrowserConfig { url: String, width: u32, height: u32, fps: u32, profile_dir: PathBuf }`
  - `p2c_browser::run(cfg: BrowserConfig, slot: FrameSlot, ready: impl FnOnce(BrowserHandle) + Send + 'static) -> anyhow::Result<()>`: initializes CEF with windowless rendering, `windowless_frame_rate = fps`, `root_cache_path = profile_dir`, device scale factor 1.0, and view rect = width×height. It creates the browser, calls `ready`, then **blocks the calling (main) thread** in the CEF message loop until `BrowserHandle::shutdown()`.
  - `BrowserHandle` (Clone + Send): `navigate(&self, url: &str)`, `shutdown(&self)`. Calls are posted to the CEF UI thread.
  - `OnPaint` (element type `View` only): copy the **whole** buffer into a new `Frame` and `slot.put` it. Ignore dirty rects for copying purposes.

- [ ] **Step 1: Pin CEF.** Add the `cef` crate at the latest release; follow its README for downloading the matching CEF binary distribution (its `export-cef-dir` tool, into `third_party/cef`) and set `CEF_PATH`. Base the code on the crate's off-screen-rendering example. Record the exact setup commands in `CLAUDE.md` under "Build". Verify: `cargo build -p p2c-browser` succeeds.
- [ ] **Step 2: Write failing integration tests** in `tests/render.rs` (`#[ignore]`; each runs CEF on the test's main thread, so put them in separate test binaries or run with `--test-threads=1`, and shut down from a watcher thread):
  - `renders_solid_color`: url `data:text/html,<body style="margin:0;background:#ff0000">`, 640×360 → within 10 s a frame of exactly 640×360 arrives whose pixel (320,180) is BGRA ≈ (0,0,255,255) (±3)
  - `partial_repaint_keeps_full_frame`: a page with a left half `#00ff00` and a right-half `<div>` whose color changes to blue after 500 ms via JS → after the change, pixel (100,180) is still green and (540,180) is blue
- [ ] **Step 3: Run** `cargo test -p p2c-browser -- --ignored --test-threads=1`. Expected: FAIL.
- [ ] **Step 4: Implement** per the interface.
- [ ] **Step 5: Run** the tests again. Expected: PASS.
- [ ] **Step 6: Commit** `feat(browser): CEF offscreen rendering into frame slot`.

### Task 7: Wire the page to the camera

**Files:**
- Modify: `crates/page2cam/src/main.rs`
- Modify: `CLAUDE.md` (build/run/test commands, prerequisites)

**Interfaces:**
- Consumes: everything above.
- Produces: the runnable `page2cam` binary.

- [ ] **Step 1: Write failing test** `missing_device_error_mentions_modprobe`: extract `fn resolve_device(cfg: &Config, sysfs_root: &Path) -> anyhow::Result<PathBuf>` (explicit `vcam_device` wins; else `find_loopback_device`). Against an empty temp sysfs, the error string contains `modprobe v4l2loopback`.
- [ ] **Step 2: Run** `cargo test -p page2cam`. Expected: FAIL.
- [ ] **Step 3: Implement** `main`: call `maybe_run_subprocess()` first. Load config, resolve the device, open the `V4l2Sink`, and spawn the `Ticker` (which owns the sink). Then `p2c_browser::run(...)` on the main thread. Ctrl-C calls `BrowserHandle::shutdown()`; after `run` returns, `ticker.stop()`. Profile dir: `<data_dir>/page2cam/cef-profile`.
- [ ] **Step 4: Run** the tests (PASS). Then the manual end-to-end check:
  1. `sudo modprobe v4l2loopback exclusive_caps=1 card_label=Page2Cam`
  2. `cargo run -p page2cam -- --url https://www.youtube.com/embed/aqz-KE-bpKQ?autoplay=1&mute=1`
  3. In Chrome on https://webcamtests.com, or in Teams/Zoom camera settings, select "Page2Cam". The video plays smoothly at 1080p and the reported fps is ≈30.
  4. Run with `--url https://does-not-exist.invalid` → the camera shows a black or error page and keeps running (no freeze).
- [ ] **Step 5: Update `CLAUDE.md`** with prerequisites (rustup, apt packages, CEF download + `CEF_PATH`, modprobe line) and commands: `cargo build`, `cargo test --workspace`, `cargo test -p <crate> <test_name>`, ignored hardware tests (`-- --ignored --test-threads=1`), `cargo clippy --workspace -- -D warnings`, `cargo run -p page2cam -- --url ...`.
- [ ] **Step 6: Commit** `feat: render web page to Linux virtual camera`.

### Task 8: `p2c-webcam`: capture the real webcam into a FrameSlot

**Files:**
- Create: `crates/p2c-webcam/Cargo.toml`, `src/lib.rs`, `src/pick.rs`, `src/capture.rs`

**Interfaces:**
- Consumes: `Frame`, `FrameSlot`
- Produces:
  - `p2c_webcam::CameraInfo { pub index: u32, pub name: String, pub path: Option<PathBuf> }`
  - `p2c_webcam::list_cameras() -> anyhow::Result<Vec<CameraInfo>>` (via `nokhwa::query`)
  - `p2c_webcam::pick_camera(cams: &[CameraInfo], wanted: Option<&str>, own_device: Option<&Path>) -> Option<CameraInfo>`: never returns a camera whose `path == own_device` or whose name contains `Page2Cam`. If `wanted` is given, it returns the first camera whose name contains it (case-insensitive); otherwise the first remaining camera.
  - `p2c_webcam::WebcamCapture::start(cam: &CameraInfo, slot: FrameSlot) -> anyhow::Result<WebcamCapture>`: opens at the highest format ≤1280×720 @ ≥24 fps (prefer MJPEG), decodes to BGRA on its own thread and `slot.put`s each frame. `stop(self)` joins. If the device errors (unplugged), log once and end the thread; the slot simply stops updating.

- [ ] **Step 1: Write failing tests** in `pick.rs`:
  - `picker_skips_own_loopback`: cams `[{0,"Page2Cam",/dev/video5}, {1,"Integrated Camera",/dev/video0}]`, own `/dev/video5` → index 1
  - `picker_skips_by_name_even_without_path`: `[{0,"Page2Cam",None}]` → `None`
  - `picker_honors_wanted_name`: `[{0,"Integrated Camera"},{1,"Logitech C920"}]`, wanted `"c920"` → index 1
  - `picker_none_when_empty`: `[]` → `None`
- [ ] **Step 2: Run** `cargo test -p p2c-webcam`. Expected: FAIL.
- [ ] **Step 3: Implement** per the interface (`nokhwa` with the `input-native` feature).
- [ ] **Step 4: Run** the tests (PASS). Manual check: a tiny example `cargo run -p p2c-webcam --example list` prints the attached cameras, excluding none, so you can see what the picker sees. (No webcam was detected on this machine during planning; plug one in.)
- [ ] **Step 5: Commit** `feat(webcam): camera listing, safe picker, capture into slot`.

### Task 9: PIP overlay in the compositor and CLI

**Files:**
- Create: `crates/p2c-compositor/src/pip.rs`
- Modify: `crates/p2c-compositor/src/compose.rs`, `src/ticker.rs`, `crates/page2cam/src/config.rs`, `src/main.rs`, `CLAUDE.md`

**Interfaces:**
- Consumes: `FrameSlot` (webcam), `pick_camera`, `WebcamCapture`, `resolve_device`
- Produces (later milestones depend on these exact names; the admin UI in milestone 3 mutates `PipLayout` live):
  - `PipLayout { pub visible: bool, pub x: f32, pub y: f32, pub width: f32 }`: `x`/`y` are the PIP's top-left corner as a fraction (0..1) of the output; `width` is a fraction of output width (height follows the webcam's aspect ratio). Default: bottom-right preset, width 0.25, visible.
  - `PipLayout::preset(corner: Corner, width: f32) -> PipLayout` with `enum Corner { TopLeft, TopRight, BottomLeft, BottomRight }` and a 2% margin from the edges.
  - `pip::pip_rect(layout: &PipLayout, cam_w: u32, cam_h: u32, out_w: u32, out_h: u32) -> Option<(u32, u32, u32, u32)>` (x, y, w, h in pixels): `None` if not visible or zero size; otherwise clamped fully inside the output.
  - `compose(page: Option<&Frame>, pip: Option<(&Frame, &PipLayout)>, out: &mut Frame)` (the signature changes from Task 3; update callers and tests).
  - `Ticker::spawn(spec, page: FrameSlot, webcam: Option<FrameSlot>, layout: Arc<RwLock<PipLayout>>, sinks)`: per tick, a webcam frame whose slot `seq` hasn't changed for 1 s counts as absent.
  - Config: `pip: PipLayout` and `webcam: Option<String>` (name filter). CLI: `--pip <top-left|top-right|bottom-left|bottom-right|off>`, `--pip-size <percent of width, 5..=50>`, `--webcam <name>`.

- [ ] **Step 1: Write failing tests:**
  - `preset_bottom_right`: `pip_rect(preset(BottomRight,0.25), 1280,720, 1920,1080)` → `(1402, 776, 480, 270)` (margin 38/22 px rounded; assert with ±1)
  - `pip_clamped_inside_frame`: layout `x=0.95,y=0.95,width=0.25`, 1920×1080 → rect fits: `x+w <= 1920 && y+h <= 1080`
  - `hidden_pip_has_no_rect`: `visible=false` → `None`
  - `compose_draws_pip_over_page`: red 64×36 page, blue 16×9 cam, layout `x=0,y=0,width=0.25` → out (2,2) blue, out (40,20) red
  - `stale_pip_is_hidden`: ticker with a webcam slot that receives one blue frame and then nothing → a sink sampling the PIP area sees blue within the first 0.5 s and page color after 1.5 s
  - `rejects_pip_size_out_of_range`: `--pip-size 80` → config validation error
- [ ] **Step 2: Run** `cargo test -p p2c-compositor -p page2cam`. Expected: FAIL.
- [ ] **Step 3: Implement.** Scale the webcam frame to the rect with `fast_image_resize` (cache the destination buffer per rect size). Copy it opaquely over the page; borders and rounded corners are left for milestone 3. In `main`, resolve the vcam device first, then `pick_camera(..., own_device=Some(&device))`. No webcam found → log a warning and run without PIP.
- [ ] **Step 4: Run** the tests (PASS). Manual check: `cargo run -p page2cam -- --url https://example.com --pip bottom-right --pip-size 25`, then in Teams/webcamtests.com your face appears bottom-right over the page. Unplug the webcam → the PIP disappears within about 1 s and the page keeps running. Try `--pip top-left` and `--pip off`.
- [ ] **Step 5: Update `CLAUDE.md`** run examples with the PIP flags.
- [ ] **Step 6: Commit** `feat: webcam picture-in-picture overlay`.
