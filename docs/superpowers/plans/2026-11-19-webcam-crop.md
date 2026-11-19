# Webcam Crop + Virtual-Camera Start Retry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the operator trim and zoom the webcam picture (in every overlay style, saved in settings), and make the Windows virtual camera start itself again when its first start fails.

**Architecture:** A `Crop` rectangle (0..1 of the camera frame) is applied in the webcam capture thread before the frame enters the webcam slot, so the segmenter, both compositors and the admin geometry work unchanged on the smaller frame. The admin page edits it through `/api/crop`. The Windows virtual-camera sink retries `MFCreateVirtualCamera(...).Start()` in the background instead of failing once.

**Tech Stack:** Rust (p2c-core, p2c-webcam, p2c-admin, p2c-vcam, page2cam), axum, single-file admin UI (`ui/index.html`), Media Foundation.

**Spec:** `docs/superpowers/specs/2026-11-19-webcam-crop-design.md` (+ the retry fix described in Task 3, from the 2026-11-19 acceptance session: Start failed with 0x8007000D while Frame Server was restarting and Page2Cam never retried).

## Global Constraints

- `Crop { x, y, w, h }` in 0..1 of the camera frame; default `{0, 0, 1, 1}`; minimum `w`/`h` 0.10; always inside the frame; pixel crop sizes even.
- A full crop does no copy; a partial crop is a row copy of the region (no scaling) — the frame entering the webcam slot is the cropped one.
- Settings key `webcam_crop = [x, y, w, h]`, saved with `toml_edit` (comments kept) like `overlay_style`.
- Every admin route keeps the token + Host guard.
- UI: single file, no external resources; theme navy `#0d1b2a`, brand blue `#0b63f0`, white.
- Zoom slider range 1×–4×; live crop updates throttled to ~10/s; saved when a drag/slider change ends.
- Virtual-camera start retry: every 5 s until it succeeds; admin status shows `"starting (retrying): <error>"` meanwhile; Linux unchanged.
- Windows-only code behind `cfg(windows)`; Linux builds; commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Crop changed while Quality cut-out runs** — frame size changes; the segmenter must reset its model state and keep producing masks (no crash, edge settles within ~1 s). Test in Task 1 (`segmenter_survives_crop_size_change` reuses the existing size-change reset path with a real crop applied by `apply_crop`).
2. **Odd camera sizes / tiny crops** (e.g. 640×480 camera, crop 0.10 wide) — even pixel sizes ≥ 2, never zero, never outside the frame. Test in Task 1 (`pixels_even_inside_and_min_size`).
3. **Hand-edited settings with nonsense crop** (`[0.9, 0.9, 0.5, 0.5]`, negative, NaN, wrong length) — clamped/defaulted, never a startup failure. Test in Task 1 (`config_crop_bad_values_are_clamped_or_default`).
4. **Webcam stalls / restarts after a crop** — crop survives a webcam reopen (shared `Arc<RwLock<Crop>>`, not stored in the capture thread). Test in Task 1 (`crop_is_read_per_frame`).
5. **Virtual camera never comes up** (driver not installed) — the "not installed" error is final (no endless retries); only Start failures are retried. Test in Task 3 (`not_installed_is_not_retried`).

---

### Task 1: `Crop` type, crop in the capture path, settings

**Files:**
- Create: `crates/p2c-core/src/crop.rs`; Modify: `crates/p2c-core/src/lib.rs`
- Modify: `crates/p2c-webcam/src/capture.rs` (`WebcamCapture::start` takes the shared crop), `crates/page2cam/src/{main.rs,config.rs,app.rs}` (`start_webcam`, `start_fake_webcam`, App field, settings)
- Test: unit tests in `crop.rs`, `config.rs`, `crates/p2c-segment/src/segmenter.rs`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy, Debug, PartialEq)] pub struct Crop { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }` with `Crop::FULL`, `Crop::is_full(&self) -> bool`, `Crop::clamp(self) -> Crop` (NaN → FULL; w,h ≥ 0.10; x,y so the rect stays in 0..1), `Crop::pixels(&self, cam_w: u32, cam_h: u32) -> (u32, u32, u32, u32)` (x,y,w,h even, w,h ≥ 2, inside).
  - `pub fn apply_crop(frame: Frame, crop: &Crop) -> Frame` in p2c-core (returns `frame` unchanged when full).
  - `pub type SharedCrop = Arc<parking_lot::RwLock<Crop>>`.
  - `WebcamCapture::start(cam: &CameraInfo, slot: FrameSlot, crop: SharedCrop)`; the fake webcam applies the same crop per published frame.
  - Config: `pub webcam_crop: [f32; 4]` default `[0.0, 0.0, 1.0, 1.0]`, parsed through `Crop::clamp` (bad values → clamped; wrong length/NaN → FULL); `config::save_crop(path, Crop)` writes the array with `toml_edit`.
  - `App.crop: SharedCrop`.

- [ ] **Step 1: Failing tests:** `crop.rs`: `full_crop_is_identity_without_copy` (same `data` pointer when full), `crop_copies_the_region` (4×4 frame with distinct pixels, crop {0.5,0,0.5,0.5} → 2×2 equal to top-right block), `pixels_even_inside_and_min_size` (640×480 with {0.95,0.95,0.10,0.10} → inside, even, ≥2; 1280×720 {0.25,0.25,0.5,0.5} → (320,180,640,360)), `clamp_rules` (NaN → FULL, w 0.01 → 0.10, x 0.95 w 0.5 → x 0.5); `config.rs`: `crop_defaults_to_full`, `config_crop_bad_values_are_clamped_or_default`, `save_crop_keeps_comments`; capture: `crop_is_read_per_frame` (a helper `fn crop_frame(frame, &SharedCrop) -> Frame` used by the capture loop; changing the shared value changes the next result); segmenter: `segmenter_survives_crop_size_change` (feeder alternates 64×36 and its `apply_crop` {0,0,0.5,0.5} output → masks keep being published, reset counted).
- [ ] **Step 2: Run** `cargo test -p p2c-core -p p2c-webcam -p p2c-segment -p page2cam` → fail.
- [ ] **Step 3: Implement**; main.rs creates the `SharedCrop` from config before starting the webcam and passes it to `start_webcam`/`start_fake_webcam` and `App`.
- [ ] **Step 4: Run** → pass; `cargo clippy --workspace --all-targets -- -D warnings`; Linux-target clippy for p2c-core.
- [ ] **Step 5: Commit** `feat: webcam crop applied at capture, saved in settings`.

---

### Task 2: Crop in the admin API and UI

**Files:**
- Modify: `crates/p2c-admin/src/{api.rs,server.rs}`, `crates/p2c-admin/ui/index.html`, `crates/page2cam/src/app.rs`

**Interfaces:**
- Consumes: Task 1 `Crop`, `SharedCrop`, `config::save_crop`, `App.crop`.
- Produces:
  - `api.rs`: `#[derive(Serialize, Deserialize)] pub struct CropDto { x, y, w, h: f32 }`; `Controls` gains `fn crop(&self) -> CropDto; fn set_crop(&self, c: CropDto, save: bool);` (clamped through `Crop::clamp`; `save` writes settings).
  - Routes: `GET /api/crop`, `POST /api/crop` body `{x,y,w,h,save}` → 200 with the clamped crop; token + Host guard.
  - `Status.webcam_aspect` is the cropped frame's aspect (it already reads the webcam slot's latest frame — verify it reflects the cropped frame; adjust if it reads elsewhere).
  - UI: "Crop" toggle on the overlay panel; in crop mode the overlay box shows edge handles (drag an edge = trim/untrim that side, overlay shrinks/grows), a zoom slider 1×–4× (shrinks the crop around its centre keeping the current crop aspect), drag inside = pan, "Reset crop"; outside crop mode behaviour unchanged. Live POSTs throttled to ~10/s with `save:false`, final POST with `save:true` on pointer-up/slider change end.

- [ ] **Step 1: Failing tests** (server.rs with the FakeControls): `crop_route_roundtrip`, `crop_route_clamps` (w 0.01 → 0.10), `crop_route_needs_token` (403 without token, state unchanged), `crop_save_flag_reaches_controls`; app: `set_crop_saves_only_when_asked`.
- [ ] **Step 2: Run** `cargo test -p p2c-admin -p page2cam` → fail.
- [ ] **Step 3: Implement** API, App, UI (vanilla JS in index.html; reuse the existing PIP geometry functions; no external resources).
- [ ] **Step 4: Run** → pass; clippy; a quick local run (`cargo run -p page2cam -- --no-open --admin-port 8797 --test-pattern --fake-webcam third_party/models/test-person.png --config <scratch toml>`) and `GET/POST /api/crop` with the printed token; kill it afterwards. Never port 8765, never the user's settings file.
- [ ] **Step 5: Commit** `feat(admin): crop the webcam from the preview (trim, zoom, pan, reset)`.

---

### Task 3: Retry starting the Windows virtual camera

**Files:**
- Modify: `crates/p2c-vcam/src/{windows_cam.rs,lib.rs}`, `crates/page2cam/src/{main.rs,app.rs}`

**Interfaces:**
- Produces:
  - `WinVcamSink::open(spec, status: CameraStatus)` no longer fails on `Owner::start()` errors: it returns a sink whose owner is `None`, sets status `"starting (retrying): <error>"`, and `push` (ticker thread, cheap check) retries `Owner::start()` at most every 5 s until it succeeds (then status cleared, mapping attempted as today). The "not installed" (`is_registered() == false`) error stays a hard `Err` from `open` (no retries).
  - Pure helper for tests: `fn retry_due(last_try: Option<Instant>, now: Instant) -> bool` (5 s rule).
  - `main.rs`/`app.rs`: the vcam status line in the admin page comes from `CameraStatus` while retrying, as it already does for driver problems.

- [ ] **Step 1: Failing tests:** `retry_due_every_5s`, `not_installed_is_not_retried` (open with the registry check injected false → `Err` containing "not installed"; make `is_registered` injectable for tests), `start_failure_reports_retrying_status` (inject an `Owner::start` failure → sink returned, status starts with "starting (retrying)").
- [ ] **Step 2: Run** `cargo test -p p2c-vcam` (Windows) → fail.
- [ ] **Step 3: Implement** (make `Owner::start` injectable via a small `fn(&()) -> Result<Owner>` / trait object field for tests; keep the MTA owner thread design).
- [ ] **Step 4: Run** `cargo test --workspace`, clippy (Windows + Linux-target set), `cargo fmt` (revert churn in untouched files like p2c-update/tests/github.rs).
- [ ] **Step 5: Commit** `fix(vcam): keep retrying to start the virtual camera`.

---

### Task 4: Stop dangling Page2Cam processes at start

**Files:**
- Create: `crates/page2cam/src/instances.rs`; Modify: `crates/page2cam/src/main.rs`

**Interfaces:**
- Produces:
  - `pub struct Proc { pub pid: u32, pub parent: u32, pub name: String, pub path: Option<PathBuf> }`
  - `pub fn to_stop(procs: &[Proc], me: u32) -> Vec<u32>` (pure): every process named `page2cam.exe` (Windows, case-insensitive) / `page2cam` (Linux) except `me` and any process whose parent chain reaches `me` (our own CEF helpers). Orphaned helpers of a dead instance and other main instances (any install path) are included.
  - `pub fn stop_others(me: u32) -> Vec<u32>` (Windows: ToolHelp32 snapshot + `TerminateProcess` (exit code 1), then wait up to 3 s for each to exit; Linux: `/proc` scan + SIGTERM, then SIGKILL after 3 s). Logs each stopped pid and path at warn level ("stopped a leftover Page2Cam process").
  - main.rs: call `stop_others(std::process::id())` in `run()` right after the `--wait-for-pid` wait and before opening the virtual camera, webcam or admin port. Skip it inside CEF subprocesses (they return earlier via `maybe_run_subprocess`).

- [ ] **Step 1: Failing tests** (pure `to_stop`): `stops_other_main_instance`, `keeps_own_helpers` (child and grandchild of `me`), `stops_orphaned_helpers_of_dead_instance` (parent pid not in the list), `ignores_other_programs`, `name_match_is_case_insensitive_on_windows`.
- [ ] **Step 2: Run** `cargo test -p page2cam instances` → fail.
- [ ] **Step 3: Implement**; manual check: start a debug instance with `--no-open --admin-port 8796 --test-pattern --config <scratch toml>`, then a second one with port 8795 → the second logs that it stopped the first; kill the second afterwards. Never touch the user's running installed/test copy during this check: run it only when `dist\test-build\page2cam.exe` and the Program Files copy are NOT running (check first; if they are, skip the manual check and say so).
- [ ] **Step 4: Run** `cargo test --workspace`, clippy (Windows + Linux-target for page2cam if it builds there; otherwise note alsa-sys), `cargo fmt`.
- [ ] **Step 5: Commit** `feat: stop leftover Page2Cam processes at start`.

---

### Task 5: Recover from webcam and camera-service failures

Evidence (2026-11-19 14:15–14:20): Windows' Frame Server slowed (webcam 20 → 0.5 fps), its watchdog fired (`WatchdogTimer Trigger … FSStream … hr 0xC00D4E24`), the webcam failed with 0x80070428 and the service stopped; Page2Cam never recovered (webcam thread exited; virtual camera gone).

**Files:** `crates/p2c-webcam/src/capture.rs`, `crates/page2cam/src/main.rs`, `crates/p2c-vcam/src/windows_cam.rs` (builds on Task 3's retry).

**Interfaces / behaviour:**
- Webcam: when `camera.frame()` fails, the capture thread closes the camera, logs once, and reopens the same camera every 3 s until it works (respecting stop requests within 500 ms); status `webcam` shows `"reconnecting: <error>"` meanwhile. Pure helper `fn reopen_due(last_try: Option<Instant>, now: Instant) -> bool` (3 s).
- Virtual camera: detect a dead Frame Server/camera (owner thread's `IMFVirtualCamera` calls failing, or the frame mapping no longer being read: DLL's frame `seq` acknowledgement absent for 5 s while frames are written — use `Owner` health query if available, else re-`Start()` probe every 5 s); then drop the owner and re-run Task 3's start-retry loop. Status `"starting (retrying): camera service restarted"`.
- [ ] **Step 1: Failing tests:** `reopen_due_every_3s`; capture loop with an injected frame source that fails twice then succeeds → frames resume, status transitions logged; vcam: injected owner health failure → retry path entered, status text.
- [ ] **Step 2–4:** run → fail; implement; `cargo test --workspace`, clippy (Windows + Linux-target where possible), fmt.
- [ ] **Step 5: Commit** `fix: reconnect the webcam and the virtual camera after camera-service failures`.

---

### Task 6: Measure the camera DLL's cost inside Frame Server

**Goal:** find out whether the DLL's per-frame work (NV12 copy or bilinear 1080p→720p scale, RGB32 conversion) can stall Frame Server under a throttled CPU — evidence first, no fix in this task.

- [ ] **Step 1:** Add a release benchmark test in `crates/p2c-shm` (`#[ignore]`): time `nv12_to_nv12` 1920×1080→1280×720 (scale), 1920×1080→1920×1080 (copy), and `nv12_to_rgb32` 1080p, median/p95 over 300 runs; record numbers in the report.
- [ ] **Step 2:** Add per-frame timing to the DLL's sample fill (`stream.rs`): every 300 frames log `fill p50/p95/max ms, size, format` to vcam.log (cheap; no per-frame logging).
- [ ] **Step 3:** Report: numbers, and whether the scale path at a throttled CPU (~60 % clock) plausibly exceeds the frame interval. Recommend (do not implement) the fix if it does — e.g. the app writing the camera frame at the consumer's requested size (DLL publishes the requested size in the header) so the DLL only copies.
- [ ] **Step 4: Commit** `perf(vcam-dll): measure per-frame cost inside Frame Server`. The new DLL requires reinstall (`install-camera.cmd`) to take effect — say so in the report.

---

### Task 7: Update the test copy for the user

- [ ] **Step 1:** `cargo build --release -p page2cam`; if no `page2cam.exe` from `dist\test-build` is running, copy `target\release\page2cam.exe` there (verify hash); otherwise report that the user must close it. The camera DLL is unchanged by this plan (verify by hash) — no reinstall.
