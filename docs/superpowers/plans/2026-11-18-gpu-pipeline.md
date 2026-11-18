# GPU Video Pipeline (Windows, Direct3D 11) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On Windows, keep page frames on the GPU from Chromium to the finished NV12 output frame (D3D11 compositing, GPU colour conversion, one readback, hardware encoder), with a GPU picker on the admin page and a CPU fallback.

**Architecture:** A new Windows-only crate `p2c-d3d` owns adapter enumeration, the D3D11 device, the shared-texture page input, the compositing/NV12 shaders and the readback ring. The ticker drives an `OutputProducer` (GPU or CPU) and hands sinks an `Output` (BGRA and/or NV12). The camera shared memory moves to v2 (NV12 payload); the camera DLL copies/scales NV12. ffmpeg gets NV12 and a probed hardware encoder.

**Tech Stack:** Rust, `windows` 0.62 (Direct3D11, Dxgi, Fxc), CEF 156 (`cef` crate =156.6.0+156.0.30), Media Foundation, ffmpeg.

**Spec:** `docs/superpowers/specs/2026-11-18-gpu-pipeline-design.md`

## Global Constraints

- Windows 11 only for the GPU path; everything D3D behind `cfg(windows)`; the workspace must still build and test on Linux and in the `cargo xwin` cross-build (CLAUDE.md commands).
- Linux behaviour unchanged: BGRA output, v4l2 YUYV sink, BGRA stream input, no new Linux tests.
- NV12 everywhere on Windows uses **BT.709 limited range** (Y 16–235, UV 16–240); camera DLL media types tagged `MFVideoTransferMatrix_BT709` + `MFNominalRange_16_235`; ffmpeg input `-colorspace bt709 -color_range tv`.
- Camera shared memory: max 1920×1080; `VERSION = 2`; mapping name/size unchanged.
- GPU readback ring depth 3 (map frame N−2).
- GPU setting in config: key `gpu`, values `"auto"` (default) | `"cpu"` | exact adapter name.
- "CPU (compatibility)" = 0.3.0 behaviour (Chromium default GPU + `on_paint`, CPU `Compositor`, DirectML on the high-performance adapter).
- Fallback triggers: D3D11 device creation fails; no accelerated paint within **5 s** of browser creation; `OpenSharedResource1` fails **10** times in a row; device removed twice.
- Mic delay = inference latency + measured output-pipeline latency, median over ~2 s, cap **250 ms** (was 150).
- Restart: self-relaunch with the same command line plus `--wait-for-pid <pid>`; new instance waits up to **10 s** for the old one to exit.
- Encoders by DXGI vendor id: `0x10DE` → `h264_nvenc`, `0x8086` → `h264_qsv`, `0x1002` → `h264_amf`, else/fail → `libx264`.
- Acceptance (user's laptop, cutout-full 1080p30, animated page, Quality): GPU output tick ≤ 5 ms p95; 0 skipped frames over 5 min quiet and ≤ 1 % during a background `cargo build`; main process < 0.5 core excluding cut-out; camera in Teams and NVENC stream to YouTube both work.
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; on Windows write commit messages to a file and use `git commit -F <file>` (PowerShell 5.1 here-strings don't pipe).
- Build env on the user's laptop: see memory `windows-dev-setup` (PATH: cargo, CMake, Ninja, rc.exe; `CEF_PATH=%LOCALAPPDATA%\cef-win`, CEF dir on PATH for CEF tests). Don't run heavy builds while the user is testing a call — ask first.

## Review Focus

1. **GPU device lost mid-call** (laptop sleep, driver update, TDR): output must continue — recreate once, then CPU fallback, camera never freezes on the last frame for more than ~1 s. Test in Task 8 (`producer_falls_back_after_repeated_device_loss`).
2. **Old camera DLL with new app** (portable copy updated by hand): admin status must say "camera driver out of date — reinstall", DLL shows its waiting picture. Tests in Task 2 (`read_rejects_v1_and_reports_version`) and Task 4 (`vcam_status_reports_old_dll`).
3. **Chromium ignores the adapter switch** (hybrid laptop): shared texture lives on another adapter → we must find and use that adapter instead of falling back to CPU. Test in Task 6 (`open_page_texture_on_matching_adapter`, ignored, Windows).
4. **Output size not 1920×1080** (e.g. 1280×720 or 3840×2160 in the settings file): stream gets the configured size, camera gets ≤1920×1080 with aspect kept. Tests in Task 3 (`camera_size_fits_1080p`) and Task 7 (`gpu_output_4k_camera_downscaled`).
5. **Remote Desktop / no hardware adapter / only "Microsoft Basic Render Driver"**: Auto must pick CPU mode with a reason, not crash. Test in Task 5 (`auto_without_hardware_adapter_is_cpu`).

---

### Task 1: Spike — Chromium shared textures on a chosen adapter (throwaway)

**Files:**
- Create: `crates/p2c-browser/examples/gpu_spike.rs` (deleted at the end of the task; findings go into the spec)

**Interfaces:** none (throwaway).

- [ ] **Step 1: Write the spike.** Initialize CEF like `p2c_browser::run` does, but add command-line switch `use-adapter-luid=<high>,<low>` (Chromium format: two hex numbers, comma-separated; try decimal `<luid64>` if hex is ignored) from an env var `SPIKE_LUID`, set `WindowInfo { windowless_rendering_enabled: 1, shared_texture_enabled: 1, .. }`, load `https://bground-all.vercel.app/back12`, implement `on_accelerated_paint`: count calls; on each, create (once) a D3D11 device on every hardware adapter (DXGI `EnumAdapters1`), try `ID3D11Device1::OpenSharedResource1(handle)` on each, record which adapter succeeds, `CopyResource` into a texture of the returned size. Print per second: paints/s, size, adapter that opened it.
- [ ] **Step 2: Run on the laptop** with `SPIKE_LUID` = NVIDIA LUID, then Intel LUID, then unset; sample `\GPU Engine(pid_<gpu-process>*)` counters per LUID during each run (script from the session's GPU checks).
Expected record for each run: paints/s (≈30), which adapter Chromium's GPU process loads, which adapter opens the handle.
- [ ] **Step 3: Write findings** into the spec's §0 (one short table) and decide the branch: switch works → plan as written; switch ignored/partial → Task 6 uses "composite on the adapter that opens the texture" (already supported by `open_page_texture_on_matching_adapter`), GPU picker steers only DirectML.
- [ ] **Step 4: Delete the example, commit the spec update** (`docs: GPU pipeline spike results`). Report results to the user.

---

### Task 2: Camera shared memory v2 (NV12)

**Files:**
- Modify: `crates/p2c-shm/src/layout.rs`, `crates/p2c-shm/src/convert.rs`, `crates/p2c-shm/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub enum PixelFormat { Bgra = 1, Nv12 = 2 }`
  - `pub const VERSION: u32 = 2;`
  - `pub unsafe fn write_frame(base: *mut u8, fmt: PixelFormat, width: u32, height: u32, data: &[u8], time_ms: u64) -> bool` (data = BGRA w*h*4, or NV12 w*h*3/2 tightly packed; width/height even for NV12)
  - `pub unsafe fn read_frame(base: *const u8, out: &mut Vec<u8>) -> Result<FrameInfo, ReadError>`; `FrameInfo { width, height, seq, time_ms, format: PixelFormat }`; `enum ReadError { NoFrame, Busy, Version { found: u32 } }`
  - `pub unsafe fn set_dll_version(base: *mut u8)` (DLL writes `VERSION` into header field `dll_version` when it creates the mapping) and `pub unsafe fn dll_version(base: *const u8) -> u32` (0 = old DLL that never set it).
  - `pub fn nv12_to_nv12(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], pitch: usize, dw: u32, dh: u32)` (copy with pitch when sizes match, else bilinear per plane)
  - `pub fn nv12_to_rgb32(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], pitch: usize, dw: u32, dh: u32)` (BT.709 limited → full-range BGRA, nearest scaling)
- Header stays 64 bytes: add `format: u32` and `dll_version: u32` from `_pad`.

- [ ] **Step 1: Write failing tests** in `layout.rs`/`convert.rs`:
  - `nv12_roundtrip`: write 4×2 NV12 (12 bytes), read back equal, `format == Nv12`, `seq == 2`.
  - `read_rejects_v1_and_reports_version`: header with magic and `version = 1` → `Err(ReadError::Version { found: 1 })`.
  - `dll_version_defaults_to_zero_and_can_be_set`.
  - `nv12_copy_respects_pitch`: 4×2 → dst pitch 6, padding bytes untouched, UV plane at `pitch*dh`.
  - `nv12_scale_1080_to_720_keeps_colour`: solid NV12 (Y=81,U=90,V=240) 1920×1080 → 1280×720 all Y within ±1 of 81, UV within ±1.
  - `nv12_to_rgb32_bt709`: Y=235,U=V=128 → `[255,255,255,255]` ±2; BT.709 red (Y=63,U=102,V=240) → `[0,0,255,255]` ±3.
  - Keep `torn_write_is_rejected`, `oversized_frame_is_refused` (adapted to new signatures).
- [ ] **Step 2: Run** `cargo test -p p2c-shm` → compile errors / failures for the new API.
- [ ] **Step 3: Implement** the API above. Remove `fill_nv12` (BT.601; replaced by Task 3's app-side conversion) only after Task 4 stops using it — keep it for now.
- [ ] **Step 4: Run** `cargo test -p p2c-shm` → all pass.
- [ ] **Step 5: Commit** `feat(shm): v2 frame format with NV12 payload and DLL version`.

---

### Task 3: NV12 frames and CPU BT.709 conversion

**Files:**
- Create: `crates/p2c-core/src/nv12.rs`; Modify: `crates/p2c-core/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct Nv12Frame { pub width: u32, pub height: u32, pub data: Vec<u8> }` (`data.len() == w*h*3/2`; w, h even) with `Nv12Frame::new(w, h, data) -> Result<_, FrameError>`
  - `pub fn bgra_to_nv12(src: &Frame, dw: u32, dh: u32) -> Nv12Frame` (BT.709 limited, bilinear downscale when sizes differ, UV = average of the 2×2 block)
  - `pub fn camera_size(w: u32, h: u32) -> (u32, u32)` — fits into 1920×1080 keeping aspect, both even; unchanged if it already fits.

- [ ] **Step 1: Failing tests:** `white_black_red_bt709` (white → Y235 U128 V128; black → Y16; red `[0,0,255,255]` → Y63 U102 V240, each ±1), `uv_is_block_average` (2×2 block of red and blue → UV average of both ±1), `camera_size_fits_1080p` (`(1920,1080)`→same, `(1280,720)`→same, `(3840,2160)`→`(1920,1080)`, `(2560,1080)`→`(1920,810)`, `(1080,1920)`→`(606,1080)`), `nv12_new_checks_length`.
- [ ] **Step 2: Run** `cargo test -p p2c-core` → fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → pass.
- [ ] **Step 5: Commit** `feat(core): NV12 frames and BT.709 conversion`.

---

### Task 4: `Output` for sinks; camera, preview and stream sinks; camera DLL v2

**Files:**
- Modify: `crates/p2c-compositor/src/ticker.rs` (Output, FrameSink, CpuProducer), `crates/p2c-compositor/src/lib.rs`, `crates/p2c-vcam/src/{lib.rs,windows_cam.rs,linux.rs}`, `crates/p2c-admin/src/preview.rs`, `crates/p2c-stream/src/{controller.rs,args.rs}`, `crates/p2c-vcam-win-source/src/win/{stream.rs,shared.rs}`

**Interfaces:**
- Consumes: Task 2 shm API, Task 3 `Nv12Frame`, `bgra_to_nv12`, `camera_size`.
- Produces:
  - `pub struct Output { pub bgra: Option<Arc<Frame>>, pub nv12: Option<Arc<Nv12Frame>>, pub camera: Option<Arc<Nv12Frame>>, pub preview: Option<Arc<Frame>> }` — `camera` is the ≤1080p NV12 for the virtual camera (same Arc as `nv12` when the output fits).
  - `pub trait FrameSink: Send { fn push(&mut self, out: &Output) -> anyhow::Result<()>; }`
  - `pub enum OutputFormat { Bgra, Nv12 }` in `OutputSpec` as `pub format: OutputFormat` (Linux `Bgra`, Windows `Nv12`; chosen in `main.rs` by `cfg!`).
  - `pub trait OutputProducer: Send { fn produce(&mut self, inputs: &TickInputs, preview_due: bool) -> anyhow::Result<Option<Output>>; fn name(&self) -> String; }` and `pub struct TickInputs { pub page: Option<Arc<Frame>>, pub cam: Option<Arc<Frame>>, pub mask: Option<Arc<Mask>>, pub layout: PipLayout }` (page is `None` on the GPU path — the GPU producer reads its own page texture).
  - `pub struct CpuProducer` wrapping `Compositor` (returns `None` when nothing changed; then the ticker re-pushes the previous `Output`).
  - `p2c_vcam::WinVcamSink` writes `out.camera` as NV12 (`write_frame(.., PixelFormat::Nv12, ..)`); status string becomes `"error: camera driver out of date — reinstall Page2Cam"` when `dll_version(base) < p2c_shm::VERSION`, exposed via `pub fn status(&self) -> Option<String>` on a shared `Arc<Mutex<Option<String>>>` returned by `open_default`.
  - Stream: `StreamConfig { .., pub pix_fmt: &'static str }` (`"bgra"` | `"nv12"`); writer writes `out.nv12` or `out.bgra`.
- DLL: reads v2; NV12 → `nv12_to_nv12`/`nv12_to_rgb32`; BGRA payload → existing `fill_*` paths; `Version` error → waiting picture + one `vcam.log` line per minute; calls `set_dll_version` after creating the mapping; media types get `MF_MT_YUV_MATRIX = MFVideoTransferMatrix_BT709`, `MF_MT_VIDEO_NOMINAL_RANGE = MFNominalRange_16_235`.

- [ ] **Step 1: Failing tests:**
  - ticker: `ticker_produces_nv12_when_asked` (spec format Nv12, 64×36 red page → sink sees `nv12` with Y≈63 and `bgra == None`), `ticker_bgra_unchanged_on_bgra_format` (existing tests ported to `Output.bgra`), `repeats_previous_output_when_unchanged` (static inputs for 300 ms → sink count ≈ 9 at 30 fps, all same Arc).
  - preview: `preview_uses_preview_or_bgra` (Output with only `preview` → slot gets it; with only `bgra` → as today).
  - stream args: `nv12_input_args` (`-pix_fmt nv12`, `-colorspace bt709`, `-color_range tv` present when `pix_fmt == "nv12"`; unchanged args for `"bgra"`).
  - vcam (Windows, unit-level with a heap buffer as mapping): `vcam_status_reports_old_dll` (mapping with `dll_version = 0` → status contains "out of date").
- [ ] **Step 2: Run** `cargo test -p p2c-compositor -p p2c-admin -p p2c-stream -p p2c-vcam` → fail.
- [ ] **Step 3: Implement** Output/FrameSink/CpuProducer, port Ticker to `Box<dyn OutputProducer>`, adapt the three sinks and the Linux v4l2 sink (uses `out.bgra`, no behaviour change), port `main.rs` construction.
- [ ] **Step 4: Implement DLL v2** in `stream.rs`/`shared.rs` as above.
- [ ] **Step 5: Run** `cargo test --workspace` (CEF dir on PATH), `cargo clippy --workspace --all-targets -- -D warnings`, and the xwin clippy command from CLAUDE.md for `p2c-vcam-win-source` → clean.
- [ ] **Step 6: Manual check** on the laptop (ask the user first): build release, install via installer build (Task 13 packaging) or `install-camera.cmd`, camera shows the page in the Windows Camera app with correct colours.
- [ ] **Step 7: Commit** `feat: NV12 output to camera and stream; camera DLL reads shm v2`.

---

### Task 5: Adapter enumeration and the `gpu` setting

**Files:**
- Create: `crates/p2c-d3d/Cargo.toml`, `crates/p2c-d3d/src/lib.rs`, `crates/p2c-d3d/src/adapters.rs`
- Modify: `crates/p2c-segment/src/{gpu.rs,runtime.rs,engine.rs,lib.rs}` (take the adapter from the caller), `crates/page2cam/src/config.rs`

**Interfaces:**
- Produces (`p2c-d3d`, the pure parts compile on every OS; DXGI calls `cfg(windows)`):
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub struct AdapterInfo { pub index: u32, pub name: String, pub vendor_id: u32, pub luid: u64, pub perf_rank: u32, pub software: bool }` (`perf_rank` = position in `EnumAdapterByGpuPreference(HIGH_PERFORMANCE)` order).
  - `pub fn enumerate() -> Vec<AdapterInfo>` (Windows; empty elsewhere).
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub enum GpuSetting { Auto, Cpu, Named(String) }` with `GpuSetting::parse(&str) -> GpuSetting` (`"auto"`, `"cpu"`, else `Named`) and `as_str()`.
  - `pub enum GpuChoice { Gpu(AdapterInfo), Cpu { reason: String } }` and `pub fn choose(list: &[AdapterInfo], s: &GpuSetting) -> (GpuChoice, Option<String> /* note */)`.
- p2c-segment: `runtime::session(model, gpu: Option<u32 /* DXGI index */>, size)`; `open_engine(dir, choice, gpu: Option<u32>)`; `gpu.rs` removed (its logic now lives in `p2c-d3d::enumerate` + `choose`).
- Config: `pub gpu: String` default `"auto"`; `validate` accepts any string.

- [ ] **Step 1: Failing tests** in `adapters.rs`:
  - `auto_prefers_best_perf_rank_hardware` (Intel rank 1, NVIDIA rank 0 → NVIDIA).
  - `auto_without_hardware_adapter_is_cpu` (only `software: true` "Microsoft Basic Render Driver" → `Cpu { reason }` containing "no hardware GPU").
  - `named_adapter_found` / `named_adapter_missing_falls_back_to_auto_with_note` (note mentions the missing name).
  - `cpu_setting_is_cpu`.
  - `setting_round_trip`.
  - Windows-only: `enumerate_lists_this_pc` (non-empty, each `index` points at the same name via plain order).
  - config: `gpu_defaults_to_auto`.
- [ ] **Step 2: Run** `cargo test -p p2c-d3d -p page2cam` → fail.
- [ ] **Step 3: Implement**; port `p2c-segment` to the caller-supplied index (the 0.3.0 behaviour — high-performance adapter — becomes `choose(&enumerate(), &GpuSetting::Auto)` in `main.rs`).
- [ ] **Step 4: Run** `cargo test --workspace`; `cargo test -p p2c-segment --test models -- --ignored` (CARGO_PROFILE_DEV_OPT_LEVEL=3) still passes.
- [ ] **Step 5: Commit** `feat(gpu): adapter enumeration and gpu setting`.

---

### Task 6: D3D11 device, page texture input, NV12 pass and readback ring

**Files:**
- Create: `crates/p2c-d3d/src/device.rs`, `crates/p2c-d3d/src/page.rs`, `crates/p2c-d3d/src/readback.rs`, `crates/p2c-d3d/src/shaders.rs`, `crates/p2c-d3d/src/hlsl/nv12.hlsl`
- Test: `crates/p2c-d3d/tests/gpu.rs` (`#[ignore]`, Windows)

**Interfaces:**
- Consumes: Task 3 `Nv12Frame`, `camera_size`; Task 5 `AdapterInfo`.
- Produces (Windows):
  - `pub struct Gpu` (D3D11 device + immediate context, BGRA support, `ID3D11Multithread::SetMultithreadProtected(TRUE)`), `Gpu::new(luid: u64) -> anyhow::Result<Gpu>`, `fn luid(&self) -> u64`.
  - `pub struct PageInput` (clone-able `Arc` inner): `PageInput::new(gpu: &Gpu, w: u32, h: u32)`; `fn on_shared_texture(&self, handle: isize) -> anyhow::Result<()>` (opens via `OpenSharedResource1`, caches by handle value, `CopyResource` into its own BGRA texture, bumps `seq`; on a handle that opens on no adapter returns `Err`); `fn seq(&self) -> u64`; `fn adapter_mismatch(&self) -> Option<u64 /* LUID that opened it */>`.
  - `pub fn open_on_matching_adapter(handle: isize, adapters: &[AdapterInfo]) -> Option<u64>` — tries each hardware adapter, returns the LUID that opens it.
  - `pub struct Readback` ring of 3: `fn submit(&mut self, gpu: &Gpu, y: &Texture, uv: &Texture)`, `fn take(&mut self) -> Option<Nv12Frame>` (maps N−2, returns `None` until filled).
  - Shaders compiled at **runtime** with `D3DCompile` from `d3dcompiler_47.dll` (shipped next to the exe by CEF; dev builds: CEF dir on PATH), cached per `Gpu`. HLSL lives in `hlsl/*.hlsl`, embedded with `include_str!` (no build-time tooling → xwin unaffected).
  - `pub fn bgra_to_nv12_gpu(gpu: &Gpu, src: &Texture, dw: u32, dh: u32) -> (Texture /*R8 Y*/, Texture /*R8G8 UV*/)` (BT.709 limited; downscale when `dw,dh` smaller).

- [ ] **Step 1: Failing ignored tests** in `tests/gpu.rs`: `device_on_each_hardware_adapter` (create `Gpu` for every `enumerate()` hardware adapter), `nv12_gpu_matches_cpu` (upload a 64×36 gradient BGRA texture, GPU NV12 vs `p2c_core::bgra_to_nv12` ±2 per byte), `nv12_gpu_downscale_matches_cpu` (3840×2160 → `camera_size` → ±3), `readback_ring_lags_two_frames` (submit 3 distinct frames → first `take()` after 3rd submit returns frame 1), `open_page_texture_on_matching_adapter` (create a shared NT-handle texture on adapter A with `D3D11_RESOURCE_MISC_SHARED_NTHANDLE | SHARED_KEYEDMUTEX`-less flags as CEF does, `open_on_matching_adapter` returns A's LUID).
- [ ] **Step 2: Run** `cargo test -p p2c-d3d --test gpu -- --ignored` → fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → pass on the laptop; `cargo clippy` + xwin clippy clean; Linux `cargo check` of the workspace unaffected.
- [ ] **Step 5: Commit** `feat(d3d): device, shared page texture, GPU NV12 and readback`.

---

### Task 7: GPU compositor (page + webcam + mask)

**Files:**
- Create: `crates/p2c-d3d/src/compose.rs`, `crates/p2c-d3d/src/hlsl/compose.hlsl`
- Modify: `crates/p2c-compositor/src/lib.rs` (re-export `fit_rect` as `pub`)
- Test: `crates/p2c-d3d/tests/gpu.rs`

**Interfaces:**
- Consumes: Task 4 `Output`, `OutputProducer`, `TickInputs`; Task 6 `Gpu`, `PageInput`, `Readback`, `bgra_to_nv12_gpu`; `p2c_compositor::{pip_rect, fit_rect, PipLayout, OverlayStyle}`; `p2c_core::{Mask, EdgeRange}`.
- Produces:
  - `pub struct GpuProducer` implementing `OutputProducer`: `GpuProducer::new(gpu: Gpu, page: PageInput, spec: OutputSpec) -> anyhow::Result<Self>`; uploads webcam (BGRA) / mask (R8) only when their Arc changes; draws page letterboxed (`fit_rect`), PIP at `pip_rect` (incl. negative x), Box opaque, cut-out = webcam × smoothstep(lo, hi, mask) premultiplied over page, cut-out without mask → Box; NV12 at output size → `Output.nv12`; camera NV12 at `camera_size` (same Arc when equal); preview 960×540 BGRA when `preview_due`.
  - `#[cfg(test)]`/`pub fn render_bgra_for_test(&mut self, inputs) -> Frame` reading back the BGRA intermediate (parity tests only).

- [ ] **Step 1: Failing ignored parity tests** (GPU BGRA vs `p2c_compositor::Compositor` on identical inputs, ±2 per channel on every pixel): `parity_box`, `parity_cutout_small`, `parity_cutout_full_overhang_left` (x = 0.0), `parity_mask_edge_range` (mask 100 with `EdgeRange{64,192}`), `parity_stale_mask_is_box` (mask `None`), `parity_letterbox_4_3_page`, and `gpu_output_4k_camera_downscaled` (spec 3840×2160 → `out.camera` is 1920×1080, `out.nv12` 3840×2160).
- [ ] **Step 2: Run** `cargo test -p p2c-d3d --test gpu -- --ignored` → fail.
- [ ] **Step 3: Implement** (one fullscreen-triangle page pass, one quad PIP pass with constants `{rect, style, edge_lo, edge_hi}`; webcam/mask samplers bilinear clamp).
- [ ] **Step 4: Run** → pass; benchmark test `bench_compose_1080p_cutout_full` prints p50/p95 GPU time (expect ≤ 5 ms p95 on the RTX).
- [ ] **Step 5: Commit** `feat(d3d): GPU compositor with cut-out mask`.

---

### Task 8: Chromium on the GPU and producer fallback

**Files:**
- Modify: `crates/p2c-browser/src/{lib.rs,handlers.rs}`, `crates/p2c-browser/tests/render.rs`, `crates/p2c-compositor/src/ticker.rs`, `crates/page2cam/src/main.rs`

**Interfaces:**
- Consumes: Task 5 `choose`, Task 6 `Gpu`, `PageInput`, `open_on_matching_adapter`, Task 7 `GpuProducer`, Task 4 `CpuProducer`.
- Produces:
  - `BrowserConfig { .., pub gpu_page: Option<GpuPage> }`, `pub struct GpuPage { pub adapter_luid: u64, pub on_texture: Arc<dyn Fn(isize, u32, u32) -> bool + Send + Sync> }` (return false = failed open). When set: switch `use-adapter-luid` (format from Task 1), `shared_texture_enabled = 1`, `on_accelerated_paint` calls `on_texture`; `on_paint` still fills the `FrameSlot` if CEF ever sends CPU paints.
  - `BrowserHandle::recreate_cpu(&self)` — closes the browser and creates a new one with `shared_texture_enabled = 0` (CPU fallback without re-initialising CEF).
  - Ticker: `Ticker::spawn(spec, page, webcam, mask, layout, producer: Box<dyn OutputProducer>, sinks, events: Sender<ProducerEvent>)`; `pub enum ProducerEvent { Failed(String) }`; on `produce` errors: retry once (device recreate inside `GpuProducer`), second consecutive failure → send `Failed(reason)` and switch to the `CpuProducer` held as fallback; pushes previous output in between so sinks never stall.
  - `main.rs`: resolves `GpuChoice`; GPU → creates `Gpu`, `PageInput`, `GpuProducer`, `GpuPage`; watchdog thread: no texture within 5 s, or 10 failed opens in a row → `recreate_cpu()` + ticker switches to CPU; status string `video` set accordingly ("NVIDIA RTX 2000 Ada" / "CPU (fallback: <reason>)" / "CPU (compatibility)").

- [ ] **Step 1: Failing tests:** ticker `producer_falls_back_after_repeated_device_loss` (fake producer erroring twice → `ProducerEvent::Failed`, frames keep flowing from the fallback within 100 ms), `producer_single_error_is_retried`; render.rs new step `shared_textures_arrive` (with `gpu_page` → ≥ 10 `on_texture` calls in 2 s; runs only on Windows with `--ignored`).
- [ ] **Step 2: Run** → fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace`; `cargo test -p p2c-browser --test render -- --ignored` (CEF on PATH); clippy + xwin clippy.
- [ ] **Step 5: Manual run** (ask the user first): `dist/test-build` with the new exe; log shows `video: GPU …`, admin preview correct; kill-test: `pnputil /restart-device` is not needed — simulate via env `P2C_FAKE_DEVICE_LOSS=1` (debug-only hook in `GpuProducer`) → log shows fallback, preview keeps moving.
- [ ] **Step 6: Commit** `feat: Chromium shared textures into the GPU compositor, with CPU fallback`.

---

### Task 9: Hardware encoder

**Files:**
- Modify: `crates/p2c-stream/src/{args.rs,controller.rs,lib.rs}`

**Interfaces:**
- Consumes: Task 5 `AdapterInfo.vendor_id`.
- Produces: `pub enum Encoder { Nvenc, Qsv, Amf, X264 }` with `fn ffmpeg_name(self) -> &'static str`; `pub fn candidates(vendor_id: Option<u32>) -> Vec<Encoder>` (vendor's encoder first, then `X264`); `pub fn probe(ffmpeg: &Path, e: Encoder) -> bool` (1-frame `-f lavfi -i color=c=black:s=256x144:r=30 -frames:v 1 -c:v <name> -f null -`, 10 s timeout); `StreamController::set_encoder(Encoder)`; `StreamConfig { .., pub encoder: Encoder }`; args per encoder: NVENC `-preset p5 -tune hq -rc cbr -b:v/-maxrate/-bufsize, -g 2*fps, -bf 2`; QSV `-preset medium -b:v/-maxrate -g -bf 2`; AMF `-quality balanced -rc cbr -b:v -g -bf 2`; x264 unchanged.

- [ ] **Step 1: Failing tests:** `nvidia_tries_nvenc_first`, `unknown_vendor_is_x264_only`, `nvenc_args_cbr_gop_bframes` (contains `-c:v h264_nvenc`, `-rc cbr`, `-g 60` at 30 fps, `-bf 2`, `-b:v 6000k`), same for qsv/amf, `x264_args_unchanged` (existing assertions), ignored `probe_finds_working_encoder` (on the laptop: NVENC → true).
- [ ] **Step 2: Run** `cargo test -p p2c-stream` → fail.
- [ ] **Step 3: Implement**; `main.rs` probes in a background thread after start (first working candidate) and calls `set_encoder`; status `encoder` field.
- [ ] **Step 4: Run** → pass; `cargo test -p p2c-stream --test rtmp_local -- --ignored` on the laptop with NVENC (needs ffmpeg.exe — use the one in `dist/test-build`).
- [ ] **Step 5: Commit** `feat(stream): hardware H.264 encoder matched to the GPU`.

---

### Task 10: Admin page GPU picker, status and restart

**Files:**
- Modify: `crates/p2c-admin/src/{api.rs,server.rs}`, `crates/p2c-admin/ui/index.html`, `crates/page2cam/src/{app.rs,main.rs}`
- Create: `crates/page2cam/src/restart.rs`

**Interfaces:**
- Produces:
  - `api.rs`: `#[derive(Serialize)] pub struct GpuInfo { pub name: String, pub kind: String /* "discrete"|"onboard"|"cpu" */ }`; `Status { .., pub video: String, pub encoder: String, pub gpu_choice: String, pub gpu_running: String, pub restart_needed: bool }`; `Controls` gains `fn gpus(&self) -> Vec<GpuInfo>; fn set_gpu_choice(&self, choice: &str) -> anyhow::Result<()>; fn restart(&self) -> anyhow::Result<()>;`.
  - Routes: `GET /api/gpus` → `[GpuInfo]` (first entry "Auto", last "CPU (compatibility)"), `POST /api/gpu` body `{"choice": "<auto|cpu|name>"}`, `POST /api/restart`. Token + Host checks as every route.
  - UI: "Video processing" `<select>` in the cut-out/overlay panel, status line `video · encoder`, "Restart now" button visible when `restart_needed`; after restart click shows "Page2Cam is restarting — a new admin page will open".
  - `restart.rs`: `pub fn relaunch_args(current: &[String], pid: u32) -> Vec<String>` (drops an existing `--wait-for-pid x`, appends `--wait-for-pid <pid>`); `pub fn wait_for_pid(pid: u32, timeout: Duration) -> bool`; CLI flag `--wait-for-pid <pid>` (hidden) handled first thing in `run()`.
  - `set_gpu_choice` saves `gpu` with `toml_edit` like `cutout_engine`.

- [ ] **Step 1: Failing tests:** server: `gpus_route_lists_auto_first_cpu_last`, `set_gpu_saves_choice`, `set_gpu_rejects_unknown_name` (400), `restart_route_calls_controls`, `gpu_routes_need_token` (401 without token); restart: `relaunch_args_append_wait_pid`, `relaunch_args_replace_old_wait_pid`; app: `restart_needed_when_saved_choice_differs`.
- [ ] **Step 2: Run** `cargo test -p p2c-admin -p page2cam` → fail.
- [ ] **Step 3: Implement**; UI by hand in `index.html` (theme colours from CLAUDE.md; no external resources).
- [ ] **Step 4: Run** → pass; manual check of the page on the laptop (ask the user first).
- [ ] **Step 5: Commit** `feat(admin): GPU picker, video/encoder status and restart`.

---

### Task 11: Mic delay includes the output pipeline

**Files:**
- Modify: `crates/page2cam/src/app.rs`, `crates/p2c-compositor/src/ticker.rs` (latency sample out)

**Interfaces:**
- Produces: `Ticker::pipeline_latency_ms(&self) -> u32` (median of the last ~60 samples of "compose start → its NV12 taken from the readback", 0 on the CPU path); `App::update_mic_delay` adds it to the inference latency before `smoothed_delay`; cap constant `MAX_MIC_DELAY_MS = 250`.

- [ ] **Step 1: Failing tests:** `mic_delay_adds_pipeline_latency` (inference 30 + pipeline 66 → 96), `mic_delay_capped_at_250`.
- [ ] **Step 2: Run** → fail. **Step 3: Implement.** **Step 4: Run** → pass.
- [ ] **Step 5: Commit** `feat: mic delay follows the GPU output latency`.

---

### Task 12: Acceptance on the user's laptop

**Files:** none new (results go into the spec and CLAUDE.md).

- [ ] **Step 1:** Ask the user for a test window. Build release, assemble `dist/test-build` (copy of the installed folder minus `Uninstall.exe`, new exe), reinstall camera DLL via `install-camera.cmd` as admin (or the new installer from Task 13).
- [ ] **Step 2:** Run 5 min quiet + 5 min with `cargo build -p page2cam --release` in the background, cutout-full, Quality, `https://bground-all.vercel.app/back12`. Record from the log: output tick p50/p95, skipped, cut-out rates; main process CPU (per-process sampling script from this session), GPU engine load per adapter.
Expected: tick ≤ 5 ms p95; skipped 0 quiet, ≤ 1 % loaded; main < 0.5 core excluding cut-out; Chromium GPU process load on the NVIDIA.
- [ ] **Step 3:** User checks Teams (camera) and a YouTube stream (NVENC shown in status).
- [ ] **Step 4:** Repeat with GPU setting "Intel UHD" and "CPU (compatibility)" (each via Restart now) — both work.
- [ ] **Step 5:** Update CLAUDE.md (architecture: `p2c-d3d`, shm v2, Output/OutputProducer, GPU setting, runtime HLSL via d3dcompiler_47, Windows native build notes) and commit `docs: GPU pipeline in CLAUDE.md`.

---

### Task 13: Package and release 0.4.0

**Files:**
- Modify: `Cargo.toml` (version `0.4.0`), `scripts/package-windows.sh` (no change expected; verify `d3dcompiler_47.dll` already shipped), `packaging/windows/installer.nsi` only if new files.

- [ ] **Step 1:** On the Linux box (or natively with NSIS installed here): `./scripts/package-windows.sh` → `dist/Page2Cam-Setup-0.4.0.exe`, zip, `SHA256SUMS`, `SHA256SUMS.sig`; signature must verify against `RELEASE_PUBLIC_KEY` (sign output prints the public key — compare).
- [ ] **Step 2:** Install on the laptop over 0.3.0; camera DLL replaced; app starts on the GPU path.
- [ ] **Step 3:** Merge to main, tag `v0.4.0`, push, create the GitHub release with the four assets (user approves the push/release first).
