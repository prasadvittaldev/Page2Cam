# Performance fixes (CPU/GPU) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Cut Page2Cam's CPU and GPU use on the user's laptop without changing what the camera shows.

**Architecture:** Targeted fixes found by a 30-minute live monitor on 2026-11-20 (engine 1.3–2.4 cores of 28 with cut-out Quality on DirectML, NVIDIA 3D ~15 %; with no webcam the engine still used ~0.26 core and NVIDIA 3D ~10 %). The Intel GPU was 30–90 % busy with other apps and the CPU clock fell to 25–60 % under machine-wide load, so all Page2Cam work stays on the NVIDIA, and Page2Cam must not be throttled as a background app.

**Tech Stack:** Rust, ONNX Runtime (`ort`, DirectML), D3D11, CEF.

**Spec:** none separate. The user asked to "monitor … and derive a solution" and to "fix all the issue hands free" (2026-11-20). This plan is the design. Evidence: the `dist/test-build/page2cam.log` stats lines of 2026-11-20 06:51–07:29 UTC.

## Global Constraints

- Output must look the same. Model-input changes must stay within ±2 levels of today's (existing `nv12_direct_matches_bgra_path`-style tests). Compositor output hashes and GPU pins stay unchanged.
- No new settings. Every change is automatic.
- Windows-only code is `cfg(windows)`. The workspace still builds on Linux and in `cargo xwin`.
- Measure before and after on this laptop with the existing ignored benches or model tests, and record the numbers in the task report. Don't run page2cam.exe while the user's Page2Cam runs.
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. DirectML session thread changes must not slow the model. Compare model ms before and after with `cargo test -p p2c-segment --test models -- --ignored --nocapture` (release-like: `CARGO_PROFILE_DEV_OPT_LEVEL=3`).
2. Crop changes while converting: no out-of-bounds read, and even-aligned crop edges for NV12.
3. Power-throttling opt-out must apply to the CEF child processes that exist later (GPU process, renderers), not only the engine.

---

### Task 1: DirectML session threads and the model input from NV12

**Files:** `crates/p2c-segment/src/runtime.rs`, `crates/p2c-segment/src/tensor.rs`, `crates/p2c-segment/src/engine.rs`

- [ ] **DirectML session:** ONNX Runtime's default thread pool runs the CPU-assigned nodes with one spinning thread per core. Give the DirectML session `with_intra_threads(1)`, `with_inter_threads(1)`, `with_intra_op_spinning(false)` and `with_inter_op_spinning(false)`, as the CPU session already does. Then measure model ms on DirectML before and after. If 1 thread is slower by more than 10 %, use 2. Test: a unit test for whatever helper picks the DirectML thread count.
- [ ] **NV12 → model input:** `nv12_to_rgb_f32_nchw` converts every source pixel from YUV to RGB and then averages, which is 921 k conversions for 1280×720 and measured 22–26 ms per mask. Average Y over the cell, and U and V over the chroma samples the cell covers, then convert once per output pixel (float maths, BT.601 limited, as `yuv_to_rgb`). The existing test (direct vs BGRA path within ±2 levels) must still pass on its tables. Add a flat-colour exactness test and an ignored timing test at 1280×720→640×360 and →256×144. Target ≤ 3 ms on this laptop.
- [ ] **RVM recurrent state:** stop cloning the four recurrent tensors into new `Tensor`s every frame (`rec()` clones shape and data). Keep the previous outputs as owned values and feed them back without the extra copy (`ort` value reuse). Behaviour is unchanged: the existing tests plus the real-model test give the same mask.
- [ ] Commit: `perf(segment): DirectML threads, NV12 input without per-pixel conversion, no state clones`.

### Task 2: Webcam capture: convert only the cropped area, and time it

**Files:** `crates/p2c-webcam/src/capture.rs`, `crates/p2c-webcam/src/decode.rs`

- [ ] Today the camera's NV12/YUYV/MJPEG frame is decoded to full-size BGRA and then cropped. For NV12 and YUYV, convert only the crop rectangle (even-aligned, as `Crop::pixels`). MJPEG still decodes in full, then crops. Test: crop+convert equals convert+crop for random even crops, including full frame and 2×2.
- [ ] Make the NV12→BGRA inner loop branch-free per row pair (one chroma lookup per 2×2 block). Add an ignored timing test at 1280×720.
- [ ] Add the capture conversion time (p50/p95) to the existing "webcam measured" 30 s log line.
- [ ] Commit: `perf(webcam): convert only the cropped area; log capture cost`.

### Task 3: Don't let Windows throttle Page2Cam as a background app

**Files:** `crates/page2cam/src/` (engine start-up; new `power.rs` if needed), `crates/p2c-browser/src/lib.rs`

- [ ] The engine opts out of Windows power throttling (EcoQoS) for itself at start: `SetProcessInformation(ProcessPowerThrottling, ControlMask = EXECUTION_SPEED, StateMask = 0)`. Log the outcome once.
- [ ] Do the same for its CEF child processes (GPU process, renderers, utility) as they appear. On the existing 30 s stats tick, walk the engine's child processes (Toolhelp snapshot, parent pid = engine) and apply the opt-out to any not yet done. Keep a set of handled pids.
- [ ] Add the Chromium switches `disable-renderer-backgrounding` and `disable-background-timer-throttling`. A renderer was seen at Idle priority with EcoQoS.
- [ ] Test: unit test for the child-pid bookkeeping (which pids get the opt-out), and a test that the switches are in the CEF command-line list. The Windows calls are compile-checked plus a log line.
- [ ] Commit: `perf: keep Page2Cam and its Chromium processes out of Windows power throttling`.

### Task 4: GPU compositor: why it redraws, and no work for nothing

**Files:** `crates/p2c-d3d/src/compose.rs`, `crates/p2c-compositor/src/ticker.rs` (stats)

- [ ] With no webcam the GPU compositor still redrew every tick (redrawn=901/901), at ~10 % NVIDIA 3D. Count why each composite happened (page seq changed, webcam changed, mask changed, layout/backdrop changed) and add the counts to the 30 s "output (GPU)" line.
- [ ] Find which change drives redraws with a still page and no webcam, using a unit test with a fake page seq and inputs. Fix any key that changes without a visible change, for example a webcam seq bump while the PIP is hidden, or a preview flag. A page that really repaints (video, animation) still redraws.
- [ ] Test: `Key` is unchanged across ticks when page seq, webcam seq, mask seq, layout and backdrop are unchanged, and the hidden-PIP case does not count webcam changes.
- [ ] Commit: `perf(d3d): log why frames are redrawn; skip redraws nothing asked for`.
