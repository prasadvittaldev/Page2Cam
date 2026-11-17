# Page2Cam — Design

Date: 2026-11-17
Status: Draft, awaiting approval

## Intent

Page2Cam turns a live, interactive web page into a video feed. It also overlays the user's real webcam as a picture-in-picture (PIP) window that can be placed anywhere. The feed goes to:

1. **A virtual camera.** Teams, Zoom, Meet and other camera-based apps list it as a normal webcam, so everyone in the call sees the page.
2. **A YouTube Live stream** (RTMP), with sound, for YouTube viewers.

The operator drives everything from an **admin interface on the same machine**. They load any URL, switch sites, and scroll, click and type in the page while it's live. The audience sees the page and never sees the admin UI.

## Decided with the user

- Any website. The audience is call participants (virtual camera) or YouTube viewers (stream).
- Both modes: "set a URL and let it run" **and** live interaction (scroll, click, type, log in, switch sites), all from the admin interface.
- Admin interface runs on the same machine as the app.
- Platforms: **Windows 11 and Linux only.** No macOS, no Windows 10.
- **No OBS dependency.** Page2Cam provides its own virtual camera.
- Stream audio: page audio and/or microphone, selectable (page only, mic only, both mixed).
- Language: Rust.

## Goals

- Page renders at a fixed output resolution, independent of screen size. Default 1920×1080 @ 30 fps; 1280×720 selectable.
- Motion-heavy pages (video, animation) look smooth at 30 fps.
- PIP webcam: position anywhere by dragging in the admin preview, resizable, with one-click show/hide. Corner presets (TL/TR/BL/BR) as shortcuts.
- Virtual camera and YouTube stream can run at the same time and show the same composited picture.
- Audio mixer for the stream: page and mic sources, each with on/off and a volume slider.
- Admin UI shows a live preview of the output. Mouse and keyboard input on the preview is forwarded to the page.

## Non-goals (for now)

- macOS, Windows 10.
- Sending page audio *into* a Teams/Zoom call. That needs a virtual audio device; in calls, people hear the user through the call app's own mic setting.
- Multiple scenes, transitions, or other OBS-style production features.
- Controlling the admin UI from another device over the network.
- Recording to a local file.
- Streaming destinations other than RTMP (YouTube first; any RTMP URL works incidentally).

## Anti-goals

- Requiring OBS or another third-party virtual-camera product.
- Visible lag or stutter that makes live interaction feel broken (target: input to visible change on the output in under ~150 ms).
- Exposing a control surface that other local web pages or other machines can drive (see Security).
- Leaking the YouTube stream key into logs, the preview, or the UI after it's entered.

## Architecture (builder's calls unless noted)

One Rust binary, organized as a Cargo workspace. The pipeline:

```
            ┌──────────────┐   BGRA frames + PCM audio
  URL/input │ browser (CEF │──────────────┐
  ─────────▶│ offscreen)   │              ▼
            └──────────────┘        ┌────────────┐   output frames   ┌─────────────┐
            ┌──────────────┐ frames │ compositor │──────────────────▶│ virtual cam │
            │ webcam       │───────▶│ (page+PIP) │        │          └─────────────┘
            └──────────────┘        └────────────┘        │          ┌─────────────┐
            ┌──────────────┐  PCM   ┌────────────┐        └─────────▶│ RTMP stream │
            │ mic          │───────▶│ audio mix  │──────────────────▶│ (ffmpeg)    │
            └──────────────┘        └────────────┘                   └─────────────┘
                        ▲ control / preview
                ┌───────┴────────┐
                │ admin server   │◀── operator's browser (127.0.0.1)
                └────────────────┘
```

Suggested crates (names are suggestions):

| Crate | Responsibility | Approach |
|---|---|---|
| `p2c-browser` | Render pages offscreen, capture audio, inject input, navigate | Embedded Chromium via CEF (`cef` crate from tauri-apps/cef-rs). Off-screen rendering `OnPaint` gives BGRA frames; `CefAudioHandler` gives page PCM. Input goes in through `SendMouseClickEvent`/`SendMouseWheelEvent`/`SendKeyEvent`. CEF needs a helper subprocess; use the same executable with subprocess dispatch. |
| `p2c-webcam` | Capture the real webcam | `nokhwa` (Media Foundation on Windows, V4L2 on Linux). |
| `p2c-compositor` | Fixed-rate composite of page + PIP into the output frame | CPU first: scale with `fast_image_resize`, blend PIP (optional rounded corners/border). Ticks at the output fps; always uses the *latest* page/webcam frame (latest-value slots, not queues). Move to GPU (`wgpu`) only if CPU cost is too high at 1080p30. |
| `p2c-vcam` | Present output frames as a webcam | **Linux:** write frames to a `v4l2loopback` device (`v4l` crate, YUYV or NV12). Detect a missing module and show install instructions (`modprobe v4l2loopback exclusive_caps=1 card_label=Page2Cam`). **Windows 11:** `MFCreateVirtualCamera` (Media Foundation, Win11 22000+). The camera's media source is a COM DLL loaded by the Windows Frame Server service, *not* in our process, so frames reach it through named shared memory plus an event written by the app. That makes it a separate `cdylib` crate (`p2c-vcam-win-source`) that has to be registered. |
| `p2c-audio` | Mic capture, mix with page audio, resample | `cpal` for the mic; mix to 48 kHz stereo f32 with per-source gain and mute; `rubato` for resampling. |
| `p2c-stream` | Encode and push RTMP | Bundled `ffmpeg` sidecar process. Raw video goes to stdin and raw PCM audio over a localhost TCP socket (works the same on both OSes). Encode with libx264 (veryfast, CBR ~4.5–6 Mbps at 1080p30, keyframe every 2 s) plus AAC 128 kbps. Use a hardware encoder when available (NVENC/QSV/VAAPI) as an optional setting. |
| `p2c-admin` | Admin HTTP + WebSocket server and UI | `axum` on `127.0.0.1`. REST for settings; WebSocket for preview frames (JPEG, ~10–15 fps) and input events. Static UI embedded with `rust-embed` and opened in the default browser at startup. |
| `page2cam` (bin) | Wiring, lifecycle, config | `tokio` runtime. Config in TOML under the OS config dir (`directories` crate). |

### Key behaviors

- **Input mapping:** the admin preview reports pointer and keyboard events in output-frame coordinates. The server maps them to page coordinates (the page viewport equals the output resolution, so it's an identity mapping; the PIP doesn't intercept input).
- **Navigation controls:** URL bar, back/forward/reload, and a short list of saved URLs (presets).
- **Status:** the admin UI shows virtual-camera state (active/missing driver), stream state (connecting/live/error with ffmpeg's last error line), and current fps.
- **Login persistence:** the CEF profile lives in the app data dir, so sites stay logged in across restarts.

### Security

- Bind only to `127.0.0.1`.
- Generate a random session token at startup and put it in the URL that opens the admin UI. Every REST and WebSocket request needs it, which stops other local web pages from driving Page2Cam through cross-site requests.
- Store the stream key in the OS keyring (`keyring` crate), not the TOML file. Never log it.

## Milestones (suggested order)

1. Linux: page → compositor → v4l2loopback, URL from config. Proves the core loop.
2. Webcam PIP with position/size/visibility.
3. Admin UI: preview, navigation, input forwarding, PIP drag.
4. Audio pipeline and RTMP streaming to YouTube.
5. Windows 11 virtual camera (`MFCreateVirtualCamera` and the shared-memory media source).
6. Packaging: Windows installer that registers the camera DLL and ships CEF and ffmpeg; Linux AppImage/deb that depends on v4l2loopback.

## Constraints and risks

- **CEF binary size** (~150–250 MB unpacked) and a build-time download step. Pin the CEF version.
- **Windows virtual camera** is the least-trodden part. Spike it early in milestone 5, in parallel if possible. The media source DLL must be registered (admin rights at install time).
- **Linux** needs `v4l2loopback` installed and loaded; some apps (Chrome/Teams web) need `exclusive_caps=1`.
- **Mirroring:** some call apps mirror the self-view. That's the call app's behavior; don't flip the output.
- **DRM-protected video** (Netflix and similar) won't render in CEF without Widevine. Out of scope.

## Left to the builder

- Exact crate boundaries, internal message types, threading model details.
- Admin UI look and frontend tech (plain HTML/JS is fine).
- Pixel format choices and conversion paths.
- Whether to start with CPU or GPU compositing beyond the guidance above.
