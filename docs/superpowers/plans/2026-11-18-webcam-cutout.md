# Webcam Cut-out Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the room behind the presenter and composite them over the live page. There are three overlay styles (Box / Cut-out small / Cut-out full), two engines (Quality = RVM, Fast = MediaPipe selfie), and automatic fallback to Fast.

**Architecture:**
- A new `p2c-segment` crate runs ONNX models on its own thread. It reads the latest webcam frame and publishes the same frame with the **alpha channel holding the mask** into a second `FrameSlot`, so frame and mask can never drift apart.
- The compositor alpha-blends that frame for cut-out styles. It falls back to the raw webcam as a box when no fresh matte exists.
- ONNX Runtime is loaded at runtime (`ort` feature `load-dynamic`). Nothing links at build time, so the cargo-xwin cross-build stays as it is.

**Tech Stack:**
- `ort =2.0.0-rc.15` (ONNX Runtime 1.31 API, `load-dynamic`, `directml` on Windows)
- ONNX Runtime shared libraries: Linux CPU, and the Windows DirectML build
- RVM `rvm_mobilenetv3_fp32.onnx`
- `onnx-community/mediapipe_selfie_segmentation_landscape`
- the `image` crate (png/jpeg) for the test asset and the `--fake-webcam` option
- `toml_edit` to save settings

**Spec:** `docs/superpowers/specs/2026-11-18-webcam-cutout-design.md`

## Global Constraints

- Windows 11 and Linux only. Windows is cross-built from Linux with `cargo xwin` (see CLAUDE.md); every task that touches Windows code must pass the xwin build and clippy.
- "Nothing extra to install": the models and ONNX Runtime libraries ship in the zip next to the exe.
- Output fps never drops because of segmentation. The ticker never waits on inference.
- A failure of the cut-out (missing model, missing runtime, inference error) must leave the app running in Box style, with the reason shown in the admin status and the log.
- Auto-fallback compares inference time against the camera frame interval, not raw mask rate:
  - it fires when inference is slower than `max(camera interval, 1/15 s)` for 3 s;
  - a manual switch re-arms it;
  - an automatic switch is never saved.
- The operator's style and engine choices are saved to `page2cam.toml`.
- Licence: the workspace becomes `GPL-3.0-or-later`. The zip includes the GPL text, RVM (GPL-3.0) and MediaPipe (Apache-2.0) attribution, and keeps the VCamSample MIT notice.
- The admin UI JS mirrors the compositor geometry exactly, as today.

## Review Focus

1. The webcam is unplugged or frozen while in a cut-out style. The overlay must disappear as the Box does today, with no frozen ghost of the person. Test in Task 3.
2. The model or runtime files are missing. The status must name the missing file, and the Box must still work. Test in Task 4.
3. Switching engine or camera mid-run must reset RVM's recurrent state. A smeared or old silhouette must not linger. Test in Task 4.
4. Webcams at 640×480 or other odd sizes, including tiny frames, must not cause a panic, and the mask must be scaled to the frame. Tests in Tasks 2 and 4.
5. Back in Box style, segmentation must stop using CPU/GPU, and it must resume on switching back. Test in Task 4.

---

### Task 1: Spike: ONNX Runtime loads both models on Linux, and the Windows build compiles

**Files:**
- Create: `crates/p2c-segment/Cargo.toml`, `crates/p2c-segment/src/lib.rs`, `crates/p2c-segment/src/runtime.rs`, `crates/p2c-segment/tests/models.rs`
- Create: `scripts/fetch-models.sh` (downloads into `third_party/models/` and `third_party/onnxruntime/{linux,windows}/`, each with a pinned sha256)
- Modify: `.gitignore` (add `/third_party/models`, `/third_party/onnxruntime`), `Cargo.toml` (workspace dep `ort`)

**Interfaces:**
- Produces:
  - `p2c_segment::runtime::init(lib_dir: &Path) -> anyhow::Result<Device>` (loads `libonnxruntime.so` / `onnxruntime.dll` from `lib_dir`; call once)
  - `pub enum Device { DirectMl, Cpu }`, with `Display` printing "GPU (DirectML)" / "CPU"
  - `runtime::session(model: &Path, prefer_gpu: bool) -> anyhow::Result<(ort::session::Session, Device)>`
  - `p2c_segment::assets_dir() -> PathBuf`: next to the exe if `models/` exists there, else `$CARGO_MANIFEST_DIR/../../third_party` (dev)

- [ ] **Step 1: Write `scripts/fetch-models.sh`.**
  - Pin exact URLs:
    - the RVM GitHub release asset `rvm_mobilenetv3_fp32.onnx` (v1.0.0);
    - the onnx-community `mediapipe_selfie_segmentation_landscape` onnx file, at a fixed revision;
    - the ONNX Runtime 1.31.x `onnxruntime-linux-x64-*.tgz` from GitHub releases;
    - the NuGet `Microsoft.ML.OnnxRuntime.DirectML` 1.31.x `.nupkg` (a zip: take `runtimes/win-x64/native/onnxruntime.dll`) and `Microsoft.AI.DirectML` (`bin/x64-win/DirectML.dll`);
    - a public-domain/CC0 portrait photo (Wikimedia Commons) as `third_party/models/test-person.jpg`.
  - On the first run, print each sha256. Hard-code them, and from then on fail on mismatch, like `package-windows.sh` does for FFmpeg.
  - If a pinned version does not exist, pick the nearest that matches ort's API 31 and ledger a ruling.
- [ ] **Step 2: Write the ignored test `tests/models.rs::both_models_load_and_report_shapes`.** It calls `runtime::init(assets_dir()/onnxruntime/linux)`, opens both models with `prefer_gpu=false`, and prints every input and output name and shape. Assert: RVM has inputs `src`, `r1i`..`r4i`, `downsample_ratio` and outputs `fgr`, `pha`, `r1o`..`r4o`; MediaPipe has exactly one input and one output. Run it and get it to pass:
  `cargo test -p p2c-segment --test models -- --ignored --nocapture`
  Expected: PASS. Record the printed MediaPipe input layout (NHWC vs NCHW, value range) in the ledger. Task 2 uses it.
- [ ] **Step 3: Implement `runtime.rs`.** `init` uses `ort::init_from(path)`.
  - `session` tries `ep::DirectML` first when `prefer_gpu && cfg!(windows)`, and returns which device it actually got.
  - Enable `ort` with `default-features = false, features = ["load-dynamic", "std", "ndarray", "tracing"]`, plus `directml` under `[target.'cfg(windows)'.dependencies]`.
- [ ] **Step 4: Prove the Windows build.**
  `PATH=$HOME/.local/bin:$PATH CEF_PATH=$HOME/.local/share/cef-win cargo xwin build -p p2c-segment --target x86_64-pc-windows-msvc`
  Expected: success.
  - If `directml` fails to build or link, drop that feature and ledger a ruling: Quality runs on the CPU on Windows, and Task 8's README says so.
- [ ] **Step 5: Commit** `feat(segment): ONNX Runtime loading and model fetch script`.

### Task 2: Pre- and post-processing (pure, platform-free)

**Files:**
- Create: `crates/p2c-segment/src/tensor.rs`

**Interfaces:**
- Produces:
  - `to_rgb_f32_nchw(frame: &Frame, w: u32, h: u32) -> Vec<f32>`: BGRA→RGB, 0..1, resized, `[1,3,h,w]`
  - `to_rgb_f32_nhwc(frame: &Frame, w: u32, h: u32) -> Vec<f32>`: same, `[1,h,w,3]`
  - `write_alpha(frame: &mut Frame, mask: &[f32], mw: u32, mh: u32)`: bilinear-upscales the 0..1 mask into the A byte of every pixel, clamped, rounded

- [ ] **Step 1: Write the failing tests:**
  - `nchw_puts_red_first`: a 2×1 BGRA frame (pure red, pure blue) at 2×1 gives `[1,0, 0,0, 0,1]`;
  - `nhwc_interleaves`: the same frame gives `[1,0,0, 0,0,1]`;
  - `write_alpha_scales_mask`: a 2×1 mask `[0,1]` written into a 640×480 frame gives A=0 at the left edge, 255 at the right edge, and about 128 in the middle column, with BGR unchanged;
  - `write_alpha_tiny_frame`: a 1×1 frame with a 256×144 mask does not panic;
  - `mask_values_clamped`: mask values −0.5 and 1.7 become 0 and 255.
- [ ] **Step 2: Run** `cargo test -p p2c-segment tensor`. Expected: FAIL (the functions don't exist).
- [ ] **Step 3: Implement.** Resize with `p2c_compositor::scale_frame` (already public).
- [ ] **Step 4: Run** `cargo test -p p2c-segment tensor`. Expected: 5 passed.
- [ ] **Step 5: Commit** `feat(segment): tensor conversion and alpha write`.

### Task 3: The compositor draws the cut-out styles

**Files:**
- Modify: `crates/p2c-compositor/src/pip.rs` (add `OverlayStyle` and the `style` field), `compose.rs` (alpha blend), `ticker.rs` (matte slot), `lib.rs` (exports)

**Interfaces:**
- Produces:
  - `pub enum OverlayStyle { Box, CutoutSmall, CutoutFull }` (Default `Box`), with `as_str` returning "box" / "cutout-small" / "cutout-full" and `parse(&str) -> Option<Self>`
  - `PipLayout { visible, x, y, width, style: OverlayStyle }`. Every existing constructor gets `style: OverlayStyle::Box`.
  - `pip_rect` keeps its signature and handles all three styles:
    - **Box:** unchanged.
    - **CutoutSmall:** width = `layout.width × out_w`, side margins as today, bottom edge exactly at `out_h` (no bottom margin), `y` ignored, `x` 0..1 across the free width.
    - **CutoutFull:** height = `out_h`, width = `out_h × cam aspect`, shrunk to fit `out_w` keeping the aspect, bottom-anchored, `x` 0..1 across the free width (centred = 0.5), no margins, `width` ignored.
  - `compose(page, pip: Option<(&Frame, &PipLayout)>, out)`: for cut-out styles it blends `out = cam·a + out·(1−a)` using the cam frame's A byte. Box ignores A (opaque).
  - `Ticker::spawn(spec, page, webcam: Option<FrameSlot>, matte: Option<FrameSlot>, layout, sinks)`. In a cut-out style it uses the matte frame if it changed within 1 s. Otherwise it draws the raw webcam **as Box style**, and if that is also stale, it draws nothing.

- [ ] **Step 1: Write the failing tests:**
  - `cutout_small_sits_on_bottom_edge`: 1280×720 cam, 1920×1080 out, width 0.25, x 1.0 gives `(1402, 810, 480, 270)`;
  - `cutout_full_covers_height_centered`: 640×480 cam, x 0.5 gives `(240, 0, 1440, 1080)`;
  - `cutout_full_wide_cam_shrinks`: 2560×720 cam gives w = 1920 and y + h = 1080;
  - `blend_uses_alpha`: a white page and a black cam with A = [0, 128, 255] across 3 pixels give out ≈ [255, 127, 0];
  - `box_ignores_alpha`: a cam with A = 0 in Box style still covers the page;
  - `ticker_cutout_falls_back_to_box_when_matte_stale` (Review Focus 1): the matte slot is never written, so the output pixel inside the box equals the raw cam colour;
  - `ticker_cutout_hides_when_both_stale`: neither slot updates for over 1 s, so the output equals the page.
- [ ] **Step 2: Run** `cargo test -p p2c-compositor`. Expected: the new tests FAIL; the existing ones compile once `style` exists.
- [ ] **Step 3: Implement.** Scale the cam with `fast_image_resize` using `ResizeOptions::use_alpha(false)` on the BGRA cam, so the mask isn't premultiplied twice. Then do a per-pixel integer blend: `(c·a + o·(255−a) + 127) / 255`.
- [ ] **Step 4: Run** `cargo test -p p2c-compositor` and `cargo test --workspace`. Expected: all pass. Fix the other crates' `PipLayout` literals (admin, page2cam) as compile errors show up.
- [ ] **Step 5: Commit** `feat(compositor): cut-out overlay styles with alpha blending`.

### Task 4: Engines, the segmenter thread and the fallback policy

**Files:**
- Create: `crates/p2c-segment/src/engine.rs` (trait + `FastEngine` + `RvmEngine`), `src/policy.rs`, `src/segmenter.rs`

**Interfaces:**
- Consumes: Task 1 `runtime::{init, session, Device}`; Task 2 `tensor::*`.
- Produces:
  - `pub enum EngineChoice { Quality, Fast }`, with `as_str` returning "quality" / "fast" and `parse`.
  - `pub trait Engine: Send`:
    - `fn infer(&mut self, frame: &Frame) -> anyhow::Result<(Vec<f32>, u32, u32)>` returns the mask and its w, h;
    - `fn reset(&mut self)`;
    - `fn device(&self) -> Device`.
  - `FastEngine::open(dir: &Path) -> Result<Self>` uses MediaPipe at 256×144, with the layout recorded in Task 1.
  - `RvmEngine::open(dir: &Path) -> Result<Self>`:
    - `src` is fed the frame resized to 640×360, with `downsample_ratio = 0.4`;
    - it carries `r1o..r4o` back into `r1i..r4i`, which start as `[1,1,1,1]` zeros, and `reset()` zeroes them;
    - `pha` is the mask.
  - `FallbackPolicy::new()`, then `observe(&mut self, infer: Duration, cam_interval: Duration, now: Instant) -> bool` returns true once inference has been slower than `max(cam_interval, 66.7 ms)` on every observation for 3 s; `rearm(&mut self)`.
  - `Segmenter::spawn(opener: Box<dyn Fn(EngineChoice) -> Result<Box<dyn Engine>> + Send>, webcam: FrameSlot, out: FrameSlot, choice: EngineChoice) -> Segmenter`. Its methods:
    - `set_enabled(bool)`: when disabled it doesn't run inference, and it sleeps on a condvar or polls every 50 ms;
    - `set_choice(EngineChoice)`: a manual switch, which re-arms the policy;
    - `status() -> SegStatus`;
    - `stop()`.
  - `pub struct SegStatus { pub requested: EngineChoice, pub active: Option<EngineChoice>, pub device: Option<Device>, pub masks_per_sec: f32, pub note: String }`. `note` is either empty or something like "Quality was too slow on this PC, switched to Fast" or "cut-out unavailable: <error>".
  - It processes each webcam `seq` only once. The camera interval is measured from webcam `seq` changes. It calls `engine.reset()` when the webcam frame size changes or the engine is switched.
  - `p2c_segment::open_engine(dir: &Path, choice: EngineChoice) -> Result<Box<dyn Engine>>` is the real opener.

- [ ] **Step 1: Write the failing tests.** Use a `FakeEngine` (a configurable sleep and a counter for resets and infers):
  - `policy_falls_back_after_3s_slow`;
  - `policy_ignores_slow_camera`: camera 1 s, inference 80 ms, so no fallback;
  - `policy_rearm_restarts_window`;
  - `segmenter_writes_alpha_into_out_slot`: a fake mask of all 1.0 gives every A = 255 and an `out` seq that advances;
  - `segmenter_auto_falls_back_and_notes_it`: a fake Quality at 120 ms and Fast at 1 ms, the camera at 30 fps for 4 s, gives `active == Fast`, `requested == Quality`, and a note that mentions "too slow";
  - `segmenter_disabled_runs_no_inference` (Review Focus 5);
  - `segmenter_resets_on_switch_and_size_change` (Review Focus 3);
  - `segmenter_open_error_reported` (Review Focus 2): the opener returns `Err("model missing: x.onnx")`, so the note contains "x.onnx", `active` is None, and `out` is never written;
  - `segmenter_handles_640x480`.
- [ ] **Step 2: Run** `cargo test -p p2c-segment`. Expected: the new tests FAIL.
- [ ] **Step 3: Implement** the policy, the segmenter, and then the two real engines.
- [ ] **Step 4: Run** `cargo test -p p2c-segment`. Expected: all pass. Then add the ignored test `tests/models.rs::engines_cut_out_person`. It loads `test-person.jpg`, runs each engine 3 times, and asserts: mean A over the centre 20% box > 200; mean A over the four 10% corner boxes < 60.
  `cargo test -p p2c-segment --test models -- --ignored`
  Expected: PASS for both engines. Log the inference ms per engine (on this CPU) in the ledger.
- [ ] **Step 5: Commit** `feat(segment): engines, segmenter thread, auto-fallback`.

### Task 5: The webcam negotiates 30 fps and logs the real rate

**Files:**
- Modify: `crates/p2c-webcam/src/capture.rs`
- Create: `crates/p2c-webcam/src/format.rs`

**Interfaces:**
- Produces:
  - `choose_format(formats: &[CameraFormat]) -> Option<CameraFormat>`, in this preference order:
    1. fps ≥ 25 (otherwise the highest fps available);
    2. among those, the resolution closest to 1280×720 by pixel count, never above 1920×1080;
    3. MJPEG over YUYV over NV12, and only formats `decode.rs` supports.
  - The capture thread logs `webcam measured {n:.1} fps` once, after 5 s.

- [ ] **Step 1: Write the failing tests:**
  - `prefers_30fps_over_resolution`: 1920×1080@5 and 1280×720@30 give 1280×720@30;
  - `prefers_mjpeg_at_equal_mode`;
  - `skips_unsupported_formats`;
  - `falls_back_to_highest_fps_when_none_reach_25`;
  - `empty_list_none`.
- [ ] **Step 2: Run** `cargo test -p p2c-webcam format`. Expected: FAIL.
- [ ] **Step 3: Implement.** In `open()`:
  - call `Camera::new` with `RequestedFormatType::AbsoluteHighestFrameRate`, then `compatible_camera_formats()`, `choose_format`, and `set_camera_requset(RequestedFormat::new::<RgbFormat>(RequestedFormatType::Exact(f)))` (nokhwa spells it that way);
  - if listing fails, keep today's `Closest` request.
- [ ] **Step 4: Run** `cargo test -p p2c-webcam` and the xwin clippy for `p2c-webcam`. Expected: pass and clean.
- [ ] **Step 5: Commit** `fix(webcam): pick a 30 fps mode and log measured fps`.

### Task 6: Admin API, UI and the mic delay

**Files:**
- Modify: `crates/p2c-admin/src/api.rs`, `server.rs`, `ui/index.html`, `crates/p2c-audio/src/mixer.rs`

**Interfaces:**
- Consumes: Task 3 `OverlayStyle`; Task 4 `EngineChoice`, `SegStatus`.
- Produces:
  - `PipDto` gains `style: String`. Unknown values are rejected with 400.
  - `Controls` gains `fn cutout_engine(&self) -> String` and `fn set_cutout_engine(&self, engine: &str) -> anyhow::Result<()>`.
  - `POST /api/cutout {engine}`.
  - `Status` gains `cutout: String` (e.g. "Quality · GPU (DirectML) · 24 masks/s" or the note) and `cutout_engine: String`.
  - `Mixer::set_mic_delay_ms(&self, ms: u32)`. The mic path is delayed by inserting `ms` of silence once (or dropping samples when it shrinks). Change it only when the difference is more than 20 ms.
- [ ] **Step 1: Write the failing tests:**
  - server `pip_style_round_trip` and `pip_style_rejects_unknown`;
  - `cutout_engine_route` (the fake records "fast");
  - `state_includes_cutout`;
  - mixer `mic_delay_inserts_silence`: with a 10 ms delay, the first 480 frames are silent and the mic samples follow.
- [ ] **Step 2: Run** `cargo test -p p2c-admin -p p2c-audio`. Expected: FAIL.
- [ ] **Step 3: Implement the server and mixer.**
- [ ] **Step 4: Update the UI:**
  - a three-way style segmented control (Box / Cut-out small / Cut-out full);
  - a Quality/Fast toggle shown only for cut-out styles;
  - the cut-out status line in the status bar.
  
  Geometry and dragging:
  - the JS `pipRect` mirrors Task 3's geometry exactly;
  - in Cut-out small, dragging changes only x (y is pinned) and the resize corner changes the width;
  - in Cut-out full, the dashed box is replaced by a small drag handle at the bottom centre that changes only x, so clicks pass through to the page.
  
  Check it by running `cargo run -p page2cam -- --test-pattern --no-open` and fetching `/` with the token to confirm the page loads. Then run the server tests.
- [ ] **Step 5: Run** `cargo test -p p2c-admin -p p2c-audio`. Expected: pass. **Commit** `feat(admin): overlay style, cut-out engine and status; mic delay`.

### Task 7: Wire it into the app, save settings, add `--fake-webcam`

**Files:**
- Modify: `crates/page2cam/src/main.rs`, `app.rs`, `config.rs`, `Cargo.toml`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - Config keys `overlay_style = "box"` and `cutout_engine = "quality"` (both validated).
  - `config::save_choice(path: &Path, key: &str, value: &str) -> anyhow::Result<()>` uses `toml_edit`, so comments and other keys are kept and the file is created if it's missing.
  - The CLI flag `--fake-webcam <image>` feeds a still image (decoded with `image`) into the webcam slot at 30 fps, in place of a real camera.
  - App behaviour:
    - `set_pip` calls `segmenter.set_enabled(style != Box)` and saves the style;
    - `set_cutout_engine` calls `set_choice` and saves the engine;
    - an automatic fallback is never saved;
    - status fills `cutout`;
    - each second the app sets the mic delay to the segmenter's measured inference latency when a cut-out style is on, and to 0 otherwise.
  - At startup, `runtime::init(assets_dir()/onnxruntime/<os>)` runs; an error goes into the cut-out status and the app continues.

- [ ] **Step 1: Write the failing tests:**
  - config `save_choice_keeps_comments`: write a file with a comment, save `overlay_style`, re-read it; the comment is kept and the value is changed;
  - `save_choice_creates_file`;
  - `rejects_bad_overlay_style`.
- [ ] **Step 2: Run** `cargo test -p page2cam`. Expected: FAIL.
- [ ] **Step 3: Implement the config and the wiring.**
- [ ] **Step 4: Verify end to end on Linux.** Run `cargo run -p page2cam -- --url https://example.com --no-open --fake-webcam third_party/models/test-person.jpg`, then with curl and the token:
  1. POST a style of `cutout-full`, wait 3 s, and GET `/api/state`. `cutout` should show the engine and masks/s > 0.
  2. Grab one preview JPEG over `/ws` (the same way as the m3–6 end-to-end check) and save it to the scratchpad.
  3. Look at it: the person is over example.com and the room is gone.
  4. Repeat for `fast` and `cutout-small`.
  5. Then rename `third_party/onnxruntime` temporarily. The app should start, `cutout` should say what is missing, and Box should still work. Restore it.
  
  Expected: all as described. `page2cam.toml` should show the saved style and engine.
- [ ] **Step 5: Run** `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and the xwin build and clippy for page2cam. Expected: all clean. **Commit** `feat: webcam cut-out in the app`.

### Task 8: Licence, packaging and docs

**Files:**
- Modify: `Cargo.toml` (`license = "GPL-3.0-or-later"`), `scripts/package-windows.sh`, `packaging/windows/README.txt`, `CLAUDE.md`
- Create: `LICENSE` (the GPL-3.0 text), `packaging/THIRD_PARTY.txt` (RVM GPL-3.0, MediaPipe Apache-2.0, ONNX Runtime MIT, DirectML licence, VCamSample MIT, CEF BSD, FFmpeg GPL)

- [ ] **Step 1: Update `package-windows.sh`.** It runs `fetch-models.sh`, then copies into the zip:
  - `onnxruntime.dll` and `DirectML.dll` next to the exe;
  - `models/rvm_mobilenetv3_fp32.onnx`, `models/selfie_landscape.onnx`;
  - `LICENSE`, `THIRD_PARTY.txt`.
  
  The app's Windows lib dir is the exe dir. Make `assets_dir()` and the `init` path agree, and ledger the layout.
- [ ] **Step 2: Update the README.** Add a "Cut-out (remove your room)" section with the three styles, Quality/Fast, what the auto-switch message means, and new checklist lines. Covered there:
  - is the person's edge clean;
  - masks/s as shown in the status;
  - does the webcam say ~30 fps in `page2cam.log`.
- [ ] **Step 3: Update CLAUDE.md.** Cover:
  - the `p2c-segment` crate;
  - the alpha-in-A matte convention;
  - `fetch-models.sh`;
  - the `--fake-webcam` option;
  - Known risks: the Windows camera now works on the user's PC; DirectML is untested.
- [ ] **Step 4: Run** `./scripts/package-windows.sh`. Expected: it produces the zip. `unzip -l` lists the onnxruntime and DirectML DLLs and both models, and `llvm-readobj --coff-imports` on page2cam.exe shows no `onnxruntime.dll` import (it's loaded dynamically).
- [ ] **Step 5: Commit** `chore: GPL-3.0 licence, ship models and ONNX Runtime in the Windows zip`.
