# GPU video pipeline (Windows, Direct3D 11) — design

Date: 2026-11-18. Status: awaiting user approval (builder check done).

## Why

Page2Cam does all video work on the CPU: Chromium renders the page on the GPU, copies every 1080p frame back to the CPU (`on_paint`), we copy it again, then composite page + webcam + cut-out mask in Rust on one thread, and the camera DLL converts BGRA to NV12 on the CPU inside Windows' Frame Server. On the user's laptop (i7-13850HX, Intel UHD + RTX 2000 Ada) that fits in 33 ms only on a quiet machine at full clock: with a throttled CPU and normal background load (Teams, WSL, Windows services) compositing took 50–175 ms per frame, ~2 of 3 output frames were skipped, and even webcam capture slowed down. Meanwhile the NVIDIA GPU sat idle.

OBS does this on the GPU: Chromium hands over each frame as a shared D3D11 texture, the scene is composited by shaders, colour conversion runs on the GPU and only the small finished NV12 frame is read back (or goes straight to a hardware encoder). We want the same, for our Windows users, so that 1080p30 with a full-screen cut-out stays smooth on an ordinary, busy, sometimes throttled laptop.

## Goals

- Page frames stay on the GPU from Chromium to the finished output frame (no per-frame BGRA readback by Chromium, no CPU compositing).
- Page + webcam + cut-out mask composited by D3D11 shaders, with the same look and geometry as today's CPU compositor (`pip_rect`, overlay styles, mask edge range, premultiplied blend).
- NV12 produced on the GPU; one readback of ~3 MB per output frame feeds the virtual camera and the stream.
- Streaming uses a hardware H.264 encoder matched to the GPU (NVENC / Intel Quick Sync / AMD AMF) via ffmpeg, falling back to libx264.
- A GPU choice on the admin page: **Auto** (default; discrete NVIDIA/AMD if present, else the onboard Intel/AMD GPU), each installed GPU by name, and **CPU (compatibility)**. Page rendering, compositing and the cut-out model all run on the chosen GPU. The choice is saved in the settings file.
- Automatic fallback to today's CPU path when the GPU path can't start or keeps failing, with the reason on the admin status line and in the log.
- Status shows what is actually used: e.g. "Video: NVIDIA RTX 2000 Ada · encoder NVENC".

## Non-goals

- macOS (decided: Windows only; Linux keeps the CPU path).
- A GPU path on Linux.
- Direct3D 12 / Vulkan / wgpu.
- Zero-copy encoding via the NVENC/AMF/QSV SDKs (we feed ffmpeg the read-back NV12 frame; good enough at 1080p30).
- Running the cut-out model directly on GPU textures (DirectML IO binding) — the mask stays a small CPU byte buffer uploaded per mask.
- GPU decoding of the webcam's MJPEG.
- Chromium popups (`<select>` dropdowns), still not composited (existing gap).

## Anti-goals (would be a failure even if it "works")

- A user without a discrete GPU, on Remote Desktop, or with an old driver ends up worse off than today. The CPU path must stay intact and selectable, and Auto must fall back to it.
- Silent degradation: the GPU path failing and nobody can tell. Every fallback is logged and shown in the admin status.
- The camera disappearing from Teams/Zoom because app and camera DLL disagree on the shared-memory format. A mismatch must show the DLL's "waiting" picture and a clear status/log message telling the user to reinstall (the installer replaces both together).
- Making the person/page look different from today's CPU compositor (geometry, colours, edge) beyond rounding.

## Constraints

- Windows 11 only for this path; must keep compiling for Linux and in the Linux→Windows cross-build (`cargo xwin`, see CLAUDE.md). All D3D code behind `cfg(windows)`.
- CEF 156 (`cef` crate =156.6.0+156.0.30): `on_accelerated_paint(…, info: &AcceleratedPaintInfo)` gives `shared_texture_handle` (Windows NT handle to a BGRA D3D11 texture), valid only during the callback, delivered on CEF's UI thread (our main thread). Shared textures are enabled per browser via `WindowInfo::shared_texture_enabled`.
- CEF can initialize only once per process, and the GPU adapter Chromium uses is fixed at startup (command-line switch). Changing the GPU therefore needs an app restart.
- The shared texture lives on the adapter Chromium renders on; our D3D11 device must be on the same adapter to open it. So Chromium must be pointed at the chosen adapter (Chromium's `--use-adapter-luid` switch — to be confirmed by the spike).
- The camera DLL runs inside Frame Server (another process, another session) and only sees the shared memory, never our GPU.
- The cut-out model (DirectML) should run on the same chosen GPU (today's `gpu::high_performance_adapter` logic becomes "the chosen adapter").
- Existing `FrameSlot` / `MaskSlot` hand-offs stay (latest value wins; no queues).

## Approaches considered

1. **Everything in one go (chosen by the user).** Shared textures, D3D11 compositor, GPU NV12, new camera shared-memory format and hardware encoder land together in one release. Fastest to the full OBS-style result; more untested Windows-only code at once, so a spike de-risks the unknown first.
2. Two stages (first GPU compositing with BGRA readback to unchanged sinks, then GPU NV12 + new DLL format + hardware encoder). Safer, two releases. Not chosen.
3. GPU compositing fed by Chromium's CPU frames (`on_paint` → upload). Rejected: pays the GPU→CPU copy in Chromium *and* a CPU→GPU upload; slower than OBS's design and defeats the purpose.

Also decided by the user: Windows only, Direct3D 11 (not wgpu), GPU choice exposed on the admin page with Auto → discrete → onboard → CPU fallback.

## Design

### 0. Spike first (throwaway, on the user's laptop)

Before building, prove on the Intel+NVIDIA laptop:
- Chromium with `shared_texture_enabled` and `--use-adapter-luid=<NVIDIA LUID>` delivers `on_accelerated_paint` at 30 fps for an animated page, and its GPU process shows load on the NVIDIA (Task Manager / GPU Engine counters), not the Intel.
- Our D3D11 device on the NVIDIA opens the handle (`ID3D11Device1::OpenSharedResource1`) and `CopyResource` works.
- Same with the Intel LUID (onboard-only machines).
If the adapter switch does not work, or only partly (e.g. only with the Windows per-app graphics preference set), **continue without stopping (user's decision)** with the fallback design: composite on whatever adapter Chromium actually uses (find it from the shared texture: open it via the adapter whose LUID matches the texture's, trying each hardware adapter), and the GPU picker then only steers the cut-out model. Report the spike results to the user either way.

### Decisions settled after review (marked "builder-check call": made by the designer, not the user — check while reading)

- **Output size** (builder-check call): the GPU path renders at the configured output size (any size `Config` accepts, 320×240 … 3840×2160). The stream gets that size. The camera gets NV12 at the configured size if ≤1920×1080, otherwise a GPU-downscaled copy that fits 1920×1080 (aspect kept) — an extra small pass, only when the output is larger than 1080p.
- **Linux untouched** (builder-check call): `Output` carries `bgra: Option<Arc<Frame>>` and `nv12: Option<Arc<Nv12Frame>>`. Linux produces BGRA only, exactly as today (v4l2 YUYV sink, stream BGRA input, no changes, no new Linux tests). Windows produces NV12 for camera and stream on both GPU and CPU paths, plus BGRA only where needed (preview in CPU mode).
- **"CPU (compatibility)" = today's behaviour (0.3.0)** (builder-check call): Chromium renders on its default GPU and hands frames over with `on_paint`, no adapter switch; the CPU compositor; the cut-out keeps DirectML on the high-performance adapter (falls back to CPU by itself as today). So the picker's LUID is passed to Chromium, the D3D11 device and DirectML only in GPU modes; CPU mode keeps the per-component logic that ships in 0.3.0. Remote Desktop / no hardware GPU: Auto ends up in CPU mode.
- **Windows CPU mode colours** (builder-check call): on Windows, CPU mode also sends NV12 (BT.709, converted in the app instead of in the DLL), because the DLL media types are tagged BT.709. This changes colours by the 601→709 difference only (a correctness fix for HD); geometry and blending are unchanged. Test baseline: the GPU *BGRA* intermediate must match today's CPU compositor (`Compositor`) within ±2 per channel; GPU NV12 must match the app's CPU BT.709 conversion within ±2.
- **Restart** (builder-check call): "Restart now" is a plain self-relaunch for both installed and portable copies: start the same exe with the same command line, then exit (single-instance handover: the new process waits up to 10 s for the old one to release the camera and port). The new instance opens a new admin tab with its new token (as at every start); the old tab shows "Page2Cam is restarting — a new admin page will open".
- **Mic delay** (builder-check call): the delay becomes inference latency + measured output pipeline latency (time from compositing a frame to its NV12 being read back; ≈ 2 frames with the N−2 ring), median over ~2 s as today; cap raised from 150 ms to 250 ms.

### 1. Adapter selection (`p2c-gpu` or inside a new `p2c-d3d` crate)

- Enumerate hardware adapters via DXGI (skip `DXGI_ADAPTER_FLAG_SOFTWARE`): name, vendor id, LUID, plain-order index (DirectML `device_id`).
- Setting `gpu` in the config file: `"auto"` (default) | `"cpu"` | an adapter name as shown in the list.
- Auto: first hardware adapter by `DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE` (discrete if present, else onboard).
- A named adapter that is no longer present → Auto, with a status note.
- The resolved adapter (LUID) is used for: Chromium's adapter switch, our D3D11 device, DirectML.

### 2. Chromium on the GPU (`p2c-browser`)

- `run()` takes the chosen adapter LUID (Windows) and adds the adapter switch; creates the browser with `shared_texture_enabled = 1` when the GPU path is on.
- `on_accelerated_paint` (main thread): ignore popups; open the handle (cache the opened texture per handle value — CEF reuses a small pool), `CopyResource` into a page texture owned by the compositor, bump a sequence number. Must be quick and never block on the ticker; use `ID3D11Multithread::SetMultithreadProtected(TRUE)` on the shared device context, or give the browser side its own deferred path (builder's call, see "left to the builder").
- With the GPU path off (CPU mode, Linux, fallback) everything is as today (`on_paint` → `FrameSlot`).

### 3. D3D11 compositor (new `p2c-d3d` crate, Windows only)

- One D3D11 device (BGRA support, multithread-protected) on the chosen adapter, owned by the GPU compositor; shared with the browser side for the copy.
- Inputs per tick: the latest page texture (from Chromium), the latest webcam frame (`FrameSlot`, uploaded to a BGRA texture only when its `seq` changes), the latest mask (`MaskSlot`, uploaded to an R8 texture only when it changes), the `PipLayout`.
- Pass 1 → 1920×1080 (output size) BGRA render target:
  - page: letterboxed exactly like `fit_rect` (black bars), bilinear;
  - PIP rect from the existing `pip_rect()` (CPU function, unchanged), incl. negative x overhang for CutoutFull;
  - Box: opaque webcam quad; cut-out with a fresh mask: webcam × alpha, alpha = mask sampled bilinearly then remapped by the mask's `EdgeRange` (same smoothstep as `EdgeRange::lut`, as shader constants), premultiplied blend over the page; cut-out without a fresh mask → Box (same 1 s staleness rules as the ticker today).
- Pass 2: RGB → NV12 into a Y (R8, full size) and a UV (R8G8, half size) render target. Colour matrix: BT.709 limited range (HD), and the camera DLL tags its media types accordingly (`MF_MT_YUV_MATRIX = MFVideoTransferMatrix_BT709`, `MF_MT_VIDEO_NOMINAL_RANGE = 16_235`); ffmpeg input tagged `-colorspace bt709 -color_range tv`.
- Pass 3 (only when the admin preview is watched, ~12 fps): downscale to 960×540 BGRA for the JPEG preview.
- Readback: ring of 3 staging textures per readback target; map frame N−2 (no GPU stall; adds ~2 frames ≈ 66 ms latency, which the mic delay accounts for).
- Skips work when no input changed (page seq, webcam seq, mask seq, layout) — as the CPU compositor does now.
- Device lost (`DXGI_ERROR_DEVICE_REMOVED`, driver update, sleep): recreate the device once; on repeated failure fall back to CPU (§6).
- The CPU compositor (`p2c-compositor::Compositor`) stays as the Linux/CPU path and as the reference for tests.

### 4. Output frames and sinks

- New output type for sinks: `Output { bgra: Option<Arc<Frame>>, nv12: Option<Arc<Nv12Frame>>, preview: Option<Arc<Frame>> }` (`Nv12Frame { width, height, data }`, Y plane then interleaved UV, tightly packed). Linux fills `bgra` only (unchanged). Windows fills `nv12` on both paths (CPU path: BGRA→NV12 in the app, BT.709 — see "Decisions settled after review").
- `FrameSink::push` takes `&Output`. Virtual camera sink writes NV12; preview sink uses `preview` (or downsizes BGRA in CPU mode as today); stream sink feeds NV12 to ffmpeg.
- `Ticker` keeps its fixed-rate pacing and timing log; it drives either the GPU or the CPU compositor behind a small trait (e.g. `trait OutputProducer { fn tick(&mut self, inputs) -> Option<Output> }`).

### 5. Virtual camera shared memory v2 (`p2c-shm`, camera DLL)

- Header `VERSION = 2`, new `format` field: `NV12` (GPU and CPU paths) — BGRA kept as a value for completeness/tests. Payload NV12, tightly packed, ≤1920×1080. Mapping size unchanged (NV12 is smaller than BGRA).
- DLL: NV12 at the requested size → copy planes with the sample's pitch; other size (e.g. 720p) → scale each plane (bilinear) in NV12; RGB32 requested → NV12→RGB32 conversion (rarely used). Media types tagged BT.709 limited.
- Version mismatch (old DLL, new app or vice versa): DLL shows its "waiting" picture and logs "frame format v2 not supported — reinstall Page2Cam" to `vcam.log`; the app reads nothing back from the DLL, so the app's admin status says "camera driver out of date — reinstall" when the mapping header's DLL-written version (new field `dll_version`, set by the DLL on create) is older than the app's.
- The installer already re-registers the DLL; the portable zip's `install-camera.cmd` too.

### 6. Fallback and switching

- Startup: resolve adapter → create D3D11 device → start Chromium with the adapter switch and shared textures. Any failure before the first page frame → log + status, restart in CPU mode for this run (Chromium without shared textures; CEF is still initialised only once because the decision is made before CEF starts). Failure detected after CEF started (e.g. no accelerated paint arriving within 5 s, or `OpenSharedResource1` failing repeatedly) → recreate the browser with `shared_texture_enabled = 0` and switch the ticker to the CPU compositor; status explains.
- The admin GPU picker writes the setting and shows "Restart Page2Cam to apply" with a **Restart now** button that relaunches the app (same relaunch mechanism as the updater).

### 7. Hardware encoder (`p2c-stream`)

- At startup (background thread), probe ffmpeg once per candidate with a 1-frame null encode: for the chosen GPU's vendor `h264_nvenc` (NVIDIA 0x10DE), `h264_qsv` (Intel 0x8086), `h264_amf` (AMD 0x1002); else `libx264`. Cache the result; status shows the encoder.
- Encoder options aligned with YouTube's recommendations already used for x264 (CBR at `video_kbps`, 2 s keyframes, B-frames where supported).
- Input `-pix_fmt nv12` (half the bandwidth of BGRA).

### 8. Admin page

- New "Video processing" select: Auto / each GPU by name / CPU (compatibility); status line under it: "GPU: NVIDIA RTX 2000 Ada · cut-out on NVIDIA · encoder NVENC" or the fallback reason; "Restart now" when the saved choice differs from the running one.
- `Controls` gains `gpus() -> Vec<GpuInfo>`, `gpu_choice()`, `set_gpu_choice(&str)`, `restart()`; `Status` gains `video: String`, `encoder: String`.

### 9. Testing and verification

- Unit tests (all platforms): NV12 conversion (CPU), shm v2 round-trip/mismatch, adapter-choice logic (pure function over a list of adapters), encoder choice by vendor, settings parsing.
- Windows GPU tests (`#[ignore]`, run on the user's laptop): the D3D11 compositor renders the same inputs as the CPU compositor and matches within ±2 per channel (Box, CutoutSmall, CutoutFull with overhang, stale mask → Box, letterbox); NV12 matches the CPU conversion within ±2.
- CEF test (`p2c-browser/tests/render.rs`, existing harness): with shared textures on, accelerated paints arrive and open on our device.
- Acceptance on the user's laptop, cut-out-full 1080p30, animated page, Quality: output tick ≤ 5 ms p95 on the GPU path; zero skipped output frames over 5 min on a quiet machine and ≤ 1 % while a full `cargo build` runs in the background; main process CPU (excluding the cut-out) < 0.5 core; the camera in Teams and a stream to YouTube via NVENC both work.

## Left to the builder

- Crate layout (`p2c-d3d` vs. growing `p2c-compositor`), shader authoring (HLSL compiled at build time with `fxc`/`d3dcompiler` vs. precompiled bytecode checked in — must work in the xwin cross-build).
- Exact threading of the shared-texture copy (multithread-protected immediate context vs. a dedicated copy on the browser thread with a keyed mutex/fence).
- Whether the preview downscale reuses the NV12 readback or its own small target.
- Scaling filter in the DLL for non-native sizes.
