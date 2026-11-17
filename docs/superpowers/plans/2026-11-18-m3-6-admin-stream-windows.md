# Milestones 3–6: Admin UI, Audio + RTMP, Windows 11 Camera, Packaging — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** A Windows 11 build (zip folder with `page2cam.exe`) and a Linux build. Each opens a local admin page where the operator loads and drives any website, places the webcam PIP, and outputs to a virtual camera and/or YouTube Live with page and/or mic audio.

**Spec:** `docs/superpowers/specs/2026-11-17-page2cam-design.md`. Builds on milestone 1 (`docs/superpowers/plans/2026-11-17-m1-linux-page-to-vcam.md`) and its interfaces (`Frame`, `FrameSlot`, `Ticker`, `FrameSink`, `PipLayout`, `BrowserHandle`).

**User direction (2026-11-18):** "go to other parts and finish it, give me an exe I will test in a PC". The user's Windows 11 PC is the only test rig for Windows-specific code.

## Global Constraints

- Windows 11 + Linux only. No OBS. Admin binds to `127.0.0.1` only; a per-launch random token is required on every request; the `Host` header must be `127.0.0.1:<port>` or `localhost:<port>` (DNS-rebinding guard).
- Never log or echo the stream key. Store it in the OS keyring only when "remember" is ticked.
- CEF owns the main thread; tokio runs on background threads.
- Slow sinks (preview JPEG, ffmpeg) never run inline on the ticker: the ticker hands frames off through a `FrameSlot` or a bounded channel and drops frames rather than block.
- The virtual camera is optional at runtime. If it fails to open, log it, show it in the admin status, and keep running.
- Failures must explain themselves: a log file next to the exe (Windows) or in the data dir, and per-component status in the admin UI.

## Review Focus

1. Admin request without/with wrong token, or a foreign `Host` → 403. Test: `rejects_missing_token`, `rejects_foreign_host`.
2. ffmpeg missing or RTMP unreachable → stream status "error: <last ffmpeg line>", the camera keeps running. Test: `missing_ffmpeg_reports_error`.
3. Stream key never appears in logs/status. Test: `redacts_stream_key`.
4. Audio source missing (no mic) or silent page → stream still has a continuous audio track (silence). Test: `mixer_fills_silence`.
5. Preview input coordinates outside the page or on a scaled preview → clamped into the page. Test: `maps_preview_coords`.

---

### Task A: Platform seam + optional camera (all targets compile)
- `p2c-vcam`: `pub fn open_default(device: Option<&Path>, spec: &OutputSpec) -> anyhow::Result<(Box<dyn FrameSink>, String /*description*/)>`. On Linux: resolve the v4l2loopback device (the modprobe hint error moves here) → `V4l2Sink`. On Windows: the Task E implementation. Elsewhere: error.
- `page2cam/main.rs`: no Linux-only imports. Camera failure → `warn!` and status `vcam: error: …`; the app continues.
- Tests: the existing `missing_device_error_mentions_modprobe` moves to `p2c-vcam`; `cargo xwin build --target x86_64-pc-windows-msvc -p page2cam` succeeds.

### Task B: Admin server + UI (milestone 3)
- New crate `p2c-admin` (axum, tokio, rust-embed, serde, jpeg-encoder, rand).
- `AdminState` (shared `Arc`): browser handle, `Arc<RwLock<PipLayout>>`, current URL, presets, status map, stream controller, audio controls.
- `pub fn serve(state, port_hint: u16) -> anyhow::Result<(SocketAddr, String /*token*/)>` spawns its own tokio runtime thread. Opens `http://127.0.0.1:<port>/?t=<token>` in the default browser (`webbrowser` crate) unless `--no-open`.
- REST (JSON, header `X-P2C-Token`): `GET /api/state`, `POST /api/navigate {url}`, `POST /api/nav {action: back|forward|reload}`, `POST /api/pip {visible,x,y,width}`, `POST /api/stream/start {url, key, remember}`, `POST /api/stream/stop`, `POST /api/audio {page_on, page_gain, mic_on, mic_gain}`.
- `GET /ws?t=<token>`: the server sends binary JPEG preview frames (960 px wide, ≤12 fps, only while a client is connected); the client sends JSON input `{t:"move"|"down"|"up"|"wheel"|"key", x,y (0..1 of preview), button, dx, dy, key, code, text, mods}`.
- `BrowserHandle` gains `back()`, `forward()`, `reload()`, `mouse_move(x,y)`, `mouse_button(x,y,button,down,clicks)`, `wheel(x,y,dx,dy)`, `key(KeyInput)`, `text(String)`. These are executed on the CEF thread through `send_*_event`.
- `PreviewSink: FrameSink` puts into a slot; an encoder thread reads it.
- UI (`crates/p2c-admin/ui/index.html`, plain HTML/JS): URL bar + back/forward/reload + presets, live preview canvas forwarding pointer/keyboard, a draggable/resizable PIP box drawn over the preview, PIP show/hide + corner buttons, Stream panel (RTMP URL default `rtmp://a.rtmp.youtube.com/live2`, key, remember, start/stop, status), Audio panel (page/mic toggles + sliders), status bar (camera, stream, webcam, fps).
- Tests: `rejects_missing_token`, `rejects_foreign_host`, `accepts_token_header`, `maps_preview_coords`, `navigate_rejects_non_http_scheme` (only http/https/file/about allowed), and a PIP drag → `PipLayout` round-trip.

### Task C: Audio (milestone 4a)
- New crate `p2c-audio`. `AudioBus`: two sources (page, mic), each a lock-protected ring of interleaved stereo f32 @ 48 kHz, capped at 500 ms (oldest dropped). `Mixer::pull(frames) -> Vec<f32>` applies per-source on/off + gain, sums, clamps to ±1, and fills shortfalls with silence.
- Page audio: CEF `AudioHandler` (`get_audio_parameters` → 48 kHz stereo; `on_audio_stream_packet` gives planar f32 → interleave → page ring). Note: while an audio handler is set, CEF doesn't play to speakers.
- Mic: `cpal` default input; convert to stereo; linear resample to 48 kHz.
- Tests: `mixer_fills_silence`, `mixer_applies_gain_and_clamps`, `interleave_planar`, `resample_linear_length`.

### Task D: RTMP stream (milestone 4b)
- New crate `p2c-stream`. `StreamController::start(cfg: StreamConfig{url, key, width, height, fps, video_bitrate_k: 4500}, mixer)`: binds a TCP listener for audio on 127.0.0.1:0, spawns ffmpeg (found next to the exe, else PATH), raw BGRA video on stdin, f32le 48 kHz stereo over TCP → libx264 veryfast CBR, g = 2 × fps, yuv420p + AAC 128k → flv to `<url>/<key>`. Writer thread fed by a bounded channel (cap 2; the ticker-side sink uses `try_send` and drops on full). Audio thread pulls the mixer every 10 ms. Status: idle | connecting | live | error(last stderr line).
- `fn ffmpeg_args(cfg, audio_port) -> Vec<String>` and `fn redact(s, key) -> String` (the key is replaced by `****`).
- Tests: `ffmpeg_args_shape`, `redacts_stream_key`, `missing_ffmpeg_reports_error`, plus an integration test (`#[ignore]`) streaming 3 s to a local `ffmpeg -listen 1 -i rtmp://127.0.0.1:19350/live/x -c copy out.flv`, then `ffprobe` shows h264 + aac.

### Task E: Windows 11 virtual camera (milestone 5) — compiles here, first runs on the user's PC
- Port from a working reference (smourier/VCamSample, MIT; Microsoft docs for `MFCreateVirtualCamera`) — do not write the COM interface set from memory.
- New crate `p2c-vcam-win-source` (`cdylib`): COM media source (`IMFActivate` → `IMFMediaSourceEx` + `IMFMediaStream2`, `IKsControl`, `IMFGetService` as the reference requires), output NV12 at the configured size/30 fps. Frames come from a named file mapping `Global\Page2CamFrames` plus an event, **created by the DLL** (it runs in the Frame Server service, which has the privilege to create `Global\` objects) with a DACL allowing Interactive Users to write. When there are no frames yet, it shows a "Page2Cam: waiting for app" pattern. It logs to `C:\ProgramData\Page2Cam\vcam.log`.
- `p2c-vcam` Windows side: `WinVcam`: `MFStartup`, `MFCreateVirtualCamera(Software, Session, CurrentUser, "Page2Cam", CLSID)`, `Start`; the sink opens the mapping (retrying until the DLL creates it), converts BGRA→NV12 and writes with a sequence header. `Remove` on drop.
- Shared header layout `#[repr(C)] struct SharedHeader { magic: u32, version: u32, width: u32, height: u32, seq: u64 }` + NV12 payload, defined once in `p2c-vcam` (shared module compiled into both crates).
- Tests (host-runnable): `bgra_to_nv12` known colors; shared header layout size. Everything COM is compile-checked only.

### Task F: Packaging (milestone 6)
- `xtask`-style script `scripts/package-windows.sh`: `cargo xwin build --release` for `page2cam.exe` + `p2c_vcam_win_source.dll`; copy the CEF Windows runtime; download a pinned static `ffmpeg.exe` (BtbN win64 GPL build) into the folder; add `install-camera.cmd` (self-elevating `regsvr32` of the DLL), `uninstall-camera.cmd`, `README.txt` with steps and troubleshooting; zip to `dist/Page2Cam-windows-x64.zip`.
- The exe embeds an application manifest (Windows 10/11 supportedOS, per-monitor DPI aware, UTF-8 code page) like cef-rs `bundle-cef-app` does.
- `scripts/package-linux.sh`: release build + CEF runtime + run script → `dist/Page2Cam-linux-x64.tar.gz`.
- Update CLAUDE.md commands.
