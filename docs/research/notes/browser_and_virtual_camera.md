# Browser (CEF off-screen rendering) and virtual camera optimization for Page2Cam

Research date: 2026-11-18. The app pins `cef` =156.6.0 (CEF 156). Page2Cam's current state comes from reading the repo:
- `crates/p2c-browser/src/handlers.rs` `on_paint` ignores `dirty_rects` and copies the full buffer with `.to_vec()` every paint.
- `lib.rs` sets only `windowless_frame_rate = fps`. Switches are only cosmetic ones plus `autoplay-policy`. There is no `shared_texture_enabled`, no `external_begin_frame_enabled` and no GPU-related switches.
- `crates/p2c-vcam-win-source/src/win/stream.rs` uses a pull model: it builds a sample in `RequestSample` and stamps it with `MFGetSystemTime()`.
  - Duration is `1/30 s`.
  - Media types are NV12/RGB32 at 1920x1080 and 1280x720 only, at 30 fps.
  - Attributes set: `MF_DEVICESTREAM_FRAMESERVER_SHARED=1`, `PINNAME_VIDEO_CAPTURE`.
  - `SetD3DManager` is ignored.
- `crates/p2c-vcam/src/windows_cam.rs` uses `MFVirtualCameraLifetime_Session` + `MFVirtualCameraAccess_CurrentUser`.

## Q1. CEF OSR performance: shared textures, external begin frame, frame rate, dirty rects, GPU vs software, switches, footprint

### Takeaway
At 1080p30 the CPU `OnPaint` path is workable. The cheap wins are:
- stop allocating a fresh 8 MB `Vec` per paint (reuse buffers or copy only the dirty rects);
- keep `windowless_frame_rate` matched to the ticker;
- optionally drive CEF with `external_begin_frame_enabled` + `SendExternalBeginFrame` from the compositor tick, so each tick gets exactly one fresh frame.

GPU shared-texture OSR (`OnAcceleratedPaint`) is the big win only if the whole pipeline stays on the GPU (compositor and camera/encoder). Page2Cam composites on the CPU, so it would add a GPU→CPU readback, plus D3D11 code on Windows and dma-buf/EGL code on Linux. Linux shared-texture OSR also had NVIDIA correctness bugs that were fixed only in M158 (Nov 2026), after the pinned CEF 156.

### Cited Findings
- **OnPaint contract (current CEF master header).** `buffer` "contains the pixel data for the whole image", is `width*height*4` BGRA, and `dirtyRects` "contains the set of rectangles in pixel coordinates that need to be repainted". `OnPaint` "is only called when CefWindowInfo::shared_texture_enabled is set to false." — [cef_render_handler.h](https://raw.githubusercontent.com/chromiumembedded/cef/master/include/cef_render_handler.h)
- **OnAcceleratedPaint contract.** The handle is a D3D11/D3D12-openable shared texture handle on Windows; on Linux it is "several planes, each with an fd". Further:
  - "The underlying implementation uses a pool to deliver frames… The handle's resource cannot be cached and cannot be accessed outside of this callback."
  - It "should be reopened each time this callback is executed and the contents should be copied to a texture owned by the client application."
  - Source: [cef_render_handler.h](https://raw.githubusercontent.com/chromiumembedded/cef/master/include/cef_render_handler.h)
- **Windows texture details.** The Windows `cef_accelerated_paint_info_t` texture "is instantiated without a keyed mutex". The `cef_window_info_t.shared_texture_enabled` comment in `cef_types_win.h` still reads "Currently only supported on Windows (D3D11)". That contradicts the render-handler header, which documents Linux dma-buf planes, so the Windows header comment looks stale. — [cef_types_win.h](https://raw.githubusercontent.com/chromiumembedded/cef/master/include/internal/cef_types_win.h)
- **External begin frame.** `external_begin_frame_enabled`: "Set to true (1) to enable the ability to issue BeginFrame requests from the client application by calling CefBrowserHost::SendExternalBeginFrame." — [cef_types_win.h](https://raw.githubusercontent.com/chromiumembedded/cef/master/include/internal/cef_types_win.h)
  - In this mode the client runs its own BeginFrame timer, and samples say "the default timing in CEF is disabled and the windowless_frame_rate setting is ignored" (cefclient `osr_renderer_settings.h`; cef-spout sample) — [cefclient osr_renderer_settings.h mirror](https://third-party-mirror.googlesource.com/cef/+/1187115cd0135e40e430b5f212561dbe152d4d70/src/tests/cefclient/browser/osr_renderer_settings.h), [cef-spout](https://github.com/fg-uulm/cef-spout)
- **External begin frame history.** It was broken in Chromium 73+ (software rendering was forced) and later fixed (CEF PR #222, issue #2618). This is older material, so confirm it on 156. — [CEF PR #222](https://bitbucket.org/chromiumembedded/cef/pull-requests/222/fix-use_external_begin_frame-support-and/diff)
- **`windowless_frame_rate`.** It is "The maximum rate in frames per second (fps) that CefRenderHandler::OnPaint will be called… The actual fps may be lower if the browser cannot generate frames at the requested rate. The minimum value is 1 and the default value is 30." It can be changed live via `SetWindowlessFrameRate`. — [cef_types.h](https://raw.githubusercontent.com/chromiumembedded/cef/master/include/internal/cef_types.h)
  - Older search snippets put the upper bound at 60. That limit doesn't appear in the current header comment, so treat it as unverified. — [opendlang mirror of cef_browser_settings_t](https://opendlang.org/library/arsd.webview.cef_browser_settings_t.windowless_frame_rate.html)
- **`windowless_rendering_enabled` in CefSettings.** It "may reduce rendering performance on some systems" if enabled without being needed. It is needed here. An opaque `background_color` disables transparent painting. — [cef_types.h](https://raw.githubusercontent.com/chromiumembedded/cef/master/include/internal/cef_types.h)
- **Shared-texture gains (qualitative only).**
  - The CEF external-texture PR reported "4k/60fps updates with large dirty rect regions are now possible with gpu acceleration enabled" — [CEF PR #158](https://bitbucket.org/chromiumembedded/cef/pull-requests/158)
  - The cef-mixer sample says shared textures improve OSR performance — [cef-mixer](https://github.com/twobrainsgmbh/cef-mixer)
  - An Electron discussion notes the CPU OnPaint path goes through FrameSinkVideoCapturer via CPU — [Electron issue 41972](https://github-redirect.dependabot.com/electron/electron/issues/41972)
  - No head-to-head benchmark numbers were found.
- **Linux shared-texture OSR (2026).**
  - CEF #4237/PR #4238 switched it to native-handle SharedImages because the mappable path broke GPU rendering on NVIDIA — [CEF #4237](https://github.com/chromiumembedded/cef/issues/4237)
  - That caused #4313, reported on CEF 156 (branch 8078), opened 2026-10-16: `OnAcceleratedPaint` frames "can be read before they're drawn on NVIDIA", because no fence is passed. It reproduced in 12–14 of 72 screenshots.
  - The fix "osr: linux: Send native-handle frames after GPU work completes" was merged 2026-11-10 for milestone M158. The thread doesn't confirm whether it was backported to 156.
  - Source: [CEF #4313](https://github.com/chromiumembedded/cef/issues/4313), [PR #4314](https://github.com/chromiumembedded/cef/pull/4314)
- **The cef-rs crate has OSR texture import helpers.** The `cef` Rust crate ships an `osr_texture_import` module with a D3D11 importer (seen in docs.rs for cef 148), which could reduce the work of adopting `OnAcceleratedPaint` on Windows. — [docs.rs cef osr_texture_import](https://docs.rs/crate/cef/latest/source/src/osr_texture_import/mod.rs), [d3d11.rs](https://docs.rs/crate/cef/latest/source/src/osr_texture_import/d3d11.rs)
- **Software OSR can under-deliver.** On a Raspberry Pi 5, "the actual call rate of OnPaint in CEF turned out to be just 14 frames per second". This is weak hardware; it is not evidence about desktop x86. — [maddevs write-up](https://maddevs.io/writeups/raspberry-pi-5-cef-or-webkit-for-off-screen-rendering/)
- **Switch sets seen in practice.** One example combines `disable-gpu`, `disable-gpu-compositing`, `off-screen-rendering-enabled`, `enable-begin-frame-scheduling` and `disable-d3d11` with OnPaint capture. No performance numbers were given, and several of these are legacy switches. — [Chromium paint-dev thread](https://groups.google.com/a/chromium.org/g/paint-dev/c/Ombay2o0ukQ)
- **Older OSR rewrite numbers.** An older CEF issue reported that after the compositor-based OSR rewrite, performance was "within a few CPU % of windowed rendering mode" (old material) — [chromiumembedded issue 1257](https://code.google.com/archive/p/chromiumembedded/issues/1257)

### Inferences
- **Allocation churn (biggest cheap win).** `on_paint` does a `to_vec()` of 1920×1080×4 ≈ 8.3 MB per paint, which is about 250 MB/s of fresh allocation plus memcpy at 30 fps.
  - Fix: keep 2–3 recycled `Frame` buffers (a pool returned by the ticker), or keep one persistent canvas and copy only `dirty_rects` into it. A plain memcpy of 8 MB costs about 1–3 ms on a modern desktop (my estimate, not sourced).
  - Expected gain: lower allocator/page-fault pressure and fewer RSS spikes. CEF still repaints internally, so this is a moderate CPU gain, not a dramatic one.
  - Caveat: dirty-rect copying needs a persistent buffer that the ticker doesn't hold while CEF writes. Use double-buffering or copy under the slot's lock.
- **External begin frame.** Enabling `external_begin_frame_enabled` and calling `SendExternalBeginFrame()` once per `Ticker` tick, via a `BrowserHandle` command on the CEF thread, aligns page frames with output frames.
  - This removes beat/judder between CEF's internal 30 Hz timer and the ticker's 30 Hz deadline, and avoids wasted paints when the ticker is slower.
  - Caveats: it was historically fragile (PR #222), so verify with the existing `render` test that partial repaints, video, and CSS animations still advance. If the CEF thread falls behind, frames are skipped rather than queued.
- **GPU shared texture is a large refactor.**
  - Benefits: lower CPU for heavy pages (video, WebGL, big CSS animations), and a path to D3D11 textures on the virtual-camera side.
  - Costs: Page2Cam's compositor, JPEG preview, ffmpeg stdin and shm camera are all CPU, so the frame must be read back (D3D11 staging copy + Map). Readback of 1080p BGRA is cheap compared with Chromium's own CPU capture path, but needs D3D11 code (Windows) and EGL/Vulkan dma-buf import (Linux).
  - Recommendation: Windows first. Defer Linux until the CEF pin is ≥ M158 because of #4313.
- **GPU vs software rendering.** Keep GPU rasterization enabled (the default). Use `--disable-gpu` / `--disable-gpu-compositing` only as an operator fallback flag for broken drivers or VMs, since software raster of a 1080p animated page is CPU-heavy.
  - Expose it as a config option. This makes failures self-explaining on the user's Windows PC, in line with the project constraint.
- **Footprint switches worth evaluating.** None of these were benchmarked in sources, so treat them as experiments to measure:
  - `--disable-background-timer-throttling`, `--disable-renderer-backgrounding` and `--disable-backgrounding-occluded-windows`. An OSR browser with no visible window may otherwise be treated as hidden and throttled.
  - Calling `WasHidden(false)` on the host.
  - Turning off unused features (extensions, spellcheck).
  - Setting `background_color` opaque, to skip alpha blending.
- **Popups.** `PET_POPUP` frames (`<select>` dropdowns) are currently dropped. `OnPopupShow`/`OnPopupSize` + compositing the popup buffer is a correctness item for interactive use, not a performance one.

### Gaps
- No quantitative benchmark (CPU % or latency) comparing OnPaint vs OnAcceleratedPaint, or external-begin-frame vs internal timer, on CEF 120+ was found.
- No source confirmed which command-line switches still have effect in CEF 156. `enable-begin-frame-scheduling` and `off-screen-frame-rate` appear only in old posts and may be no-ops now.
- It is unconfirmed whether the #4313 fix was backported to CEF 156/157 branches.
- The CEF GeneralUsage wiki (moved from Bitbucket to GitHub) was not fetched, so I have no current official "OSR best practices" text beyond the headers.

## Q2. Running CEF off the main thread: `multi_threaded_message_loop` vs `do_message_loop_work` pumping

### Takeaway
Officially, a manual `CefDoMessageLoopWork()` loop is "not recommended for most users". `multi_threaded_message_loop` (Windows and Linux only) or `CefRunMessageLoop` are the recommended modes, and either lets CEF schedule its own work instead of being polled. For Page2Cam, `multi_threaded_message_loop=true` on both platforms would free the main thread and remove pump-latency and busy-wait trade-offs. The cost: every CEF call must be posted to the CEF UI thread (`CefPostTask(TID_UI)`), and shutdown ordering gets stricter.

### Cited Findings
- `multi_threaded_message_loop`: "Set to true (1) to have the browser process message loop run in a separate thread. If false (0) then use CefRunMessageLoop() or call CefDoMessageLoopWork() regularly… This option is only supported on Windows and Linux." — [cef_types.h](https://raw.githubusercontent.com/chromiumembedded/cef/master/include/internal/cef_types.h)
- `external_message_pump`: it is "recommended for use in combination with the CefDoMessageLoopWork() function in cases where the CEF message loop must be integrated into an existing application message loop". "Scheduling callbacks supplement regular CefDoMessageLoopWork() calls; the application must continue to schedule calls even when no callback is received. Enabling this option is not recommended for most users; leave this option disabled and use either the CefRunMessageLoop() function or multi_threaded_message_loop if possible." — [cef_types.h](https://raw.githubusercontent.com/chromiumembedded/cef/master/include/internal/cef_types.h)
- CEF headers describe `CefRunMessageLoop` as giving "the best balance between performance and CPU usage". `CefDoMessageLoopWork` "is not recommended for most users", and the caller must "balance performance against excessive CPU usage". — summarized in search results quoting [cef_app_win.h docs (121.3)](https://cef-builds.spotifycdn.com/docs/121.3/cef__app__win_8h.html) and [cefpython issue 246](https://github-users.yhajoa.workers.dev/cztomczak/cefpython/issues/246)
- A forum report says multi-threaded message loop is recommended over external message pump on Windows for performance — [BriskBard forum](https://www.briskbard.com/forum/viewtopic.php?p=310)
- bevy_cef (Rust) uses the multi-threaded loop on Windows and an external pump elsewhere — [bevy_cef message_loop.rs](https://docs.rs/crate/bevy_cef/0.9.1/source/src/common/message_loop.rs)
- Known multi-threaded-loop caveats come from GUI-toolkit integration (WinForms controls, focus/keys); Page2Cam has no native UI — [CefGlue group](https://groups.google.com/g/cefglue/c/h96vyIBYScQ), [CefSharp #1748](https://github.1485827954.workers.dev/cefsharp/CefSharp/issues/1748)

### Inferences
- **Why it helps here.** Page2Cam's main thread exists only to pump CEF. Either option removes the polling loop's latency vs CPU knob:
  - `multi_threaded_message_loop` (main thread then just waits for shutdown);
  - `run_message_loop()` + posting commands as CEF tasks (`post_task(ThreadId::UI, ...)`) instead of an mpsc channel polled between pumps.
- **Expected gains.** Lower idle CPU, and lower input-to-paint latency (no sleep quantum between pumps). Paint cadence is also smoother, because CEF's scheduler fires `BeginFrame` on time instead of whenever the pump runs.
- **Caveats.**
  - With `multi_threaded_message_loop`, `CefShutdown` must be called from the main thread after the browser is closed.
  - Every `BrowserHost` call should go through `post_task(TID_UI)`; cef-rs exposes task wrappers.
  - Linux GTK/X11 integration differs. Page2Cam is OSR with no windows, so the risk is low but should be tested.
  - The existing `harness = false` render tests must be adapted.

### Gaps
- No benchmark comparing these modes for OSR was found.
- I did not check cef-rs 156's ergonomic support for `post_task` and multi-threaded loop samples.

## Q3. Windows 11 virtual camera: D3D samples, preferred formats, Teams remote participants, lifetime, naming, reliability

### Takeaway
Page2Cam's DLL already follows most of Microsoft's custom media source rules: QPC timestamps via `MFGetSystemTime`, the `PINNAME_VIDEO_CAPTURE` category, shared stream, and allocator-based samples.

Most likely improvements:
1. Offer more media types: lower resolutions such as 640x480/640x360/960x540, and YUY2 alongside NV12. Conferencing apps often negotiate lower or other modes than 1080p.
2. Honor `SetD3DManager` by producing `MFCreateDXGISurfaceBuffer` NV12 textures when a DXGI manager is provided. Microsoft documents this path; VCamSample uses it for the Windows Camera app, while Chrome/Edge/Teams ImageCapture used CPU.
3. Make the producer robust to Frame Server restarts and to sleep/resume.

The "Teams remote participants don't see it" problem has no documented root cause anywhere. VCamSample's author says he doesn't know why. Treat it as unverified and test it explicitly.

### Cited Findings
- **MFCreateVirtualCamera semantics.**
  - `Session`: "when the returned IMFVirtualCamera object is disposed or IMFVirtualCamera::Shutdown is called, the virtual camera will no longer be enumerable or activatable". Use `System` "to persist across sessions and/or across reboots".
  - `CurrentUser`: the camera is only for the calling user; `AllUsers` requires admin.
  - The friendly name gets "Windows Virtual Camera" appended automatically.
  - `E_ACCESSDENIED` if camera privacy denies access.
  - The same parameters reopen the same camera.
  - Packaged apps must not call it on the UI thread (consent-dialog deadlock).
  - Source: [MFCreateVirtualCamera (Microsoft Learn)](https://learn.microsoft.com/en-us/windows/win32/api/mfvirtualcamera/nf-mfvirtualcamera-mfcreatevirtualcamera)
- **Microsoft's VirtualCamera sample.**
  - Session = removed when the app closes; System = persists until reboot or manual removal. The sample caches System cameras in a config file and reloads them.
  - It recommends triggering camera consent early or calling the API on a background thread, and optionally a background service to relaunch the app at startup.
  - For debugging, attach to FrameServerMonitor before start, or to FrameServer once streaming.
  - The README documents no friendly-name/icon API.
  - Source: [microsoft/Windows-Camera VirtualCamera](https://github.com/microsoft/Windows-Camera/tree/master/Samples/VirtualCamera)
- **Frame Server custom media source rules (Microsoft Learn, updated 2025-05).**
  - Streams must use `PINNAME_VIDEO_CAPTURE` ("PINNAME_VIDEO_PREVIEW isn't supported").
  - `MF_DEVICESTREAM_STREAM_ID` is mandatory.
  - `MF_DEVICESTREAM_FRAMESERVER_SHARED=1` marks a stream shareable; with one stream it's shared by default.
  - "Each IMFSample must have the sample time and sample duration set. All sample timestamps must be based on QPC time", using `MFGetSystemTime`.
  - Buffers must come from MF allocators. For uncompressed system-memory frames, `MFCreate2DMediaBuffer` is recommended. For GPU, `MFCreateDXGISurfaceBuffer`, with the surface created from the DXGI manager passed via `IMFMediaSourceEx::SetD3DManager`.
  - Source: [Frame Server Custom Media Source](https://learn.microsoft.com/en-us/windows-hardware/drivers/stream/frame-server-custom-media-source)
- **VCamSample README.**
  - With a D3D manager (Windows 11 Camera app), frames are created and converted on the GPU. Without one ("as in ImageCapture in Chrome, Edge, or Teams"), they are generated on the CPU.
  - It offers RGB32 and NV12 "since most setups prefer NV12".
  - The DLL must be readable by Frame Server (Local Service) and Frame Server Monitor (Local System), so don't register it from a user-profile path; unblock the Mark of the Web.
  - On Teams: the camera "is shown in the preview window of apps such as Microsoft Teams" but not sent to the other participant, and the author doesn't know why.
  - Source: [smourier/VCamSample](https://github.com/smourier/VCamSample)
- **VCamSample issues (all 18 closed as of the fetch).**
  - #4: a Frame Server crash (c0000374/c0000005) was traced to a double release of an `IWICBitmapLock` in the source DLL and fixed in commit 62654a6 (Nov 2024). A refcount bug in the DLL crashes the shared Frame Server process. — [VCamSample #4](https://github.com/smourier/VCamSample/issues/4)
  - #5 (Oct 2024): after Zoom opened the camera, Teams got a blank feed. It was closed for inactivity, and the maintainer says sharing behavior depends on the client app. — [VCamSample #5](https://github.com/smourier/VCamSample/issues/5)
  - #13 (Apr 2025): the camera was "busy or blocked" in Chrome/Meet until Chrome's Media Foundation capture was disabled. The maintainer could not reproduce it with defaults. — [VCamSample #13](https://github.com/smourier/VCamSample/issues/13)
- **Exclusive vs shared access.** A full source made with `MFCreateDeviceSource` is exclusive and locks out other apps, while `IMFVirtualCamera::GetMediaSource` returns a reduced-function shared client. This was relayed via a search summary of Microsoft Learn's GetMediaSource page. — [IMFVirtualCamera::GetMediaSource](https://learn.microsoft.com/en-us/windows/win32/api/mfvirtualcamera/nf-mfvirtualcamera-imfvirtualcamera-getmediasource)
- **Teams format and resolution behavior (community reports).**
  - Teams opening a camera in exclusive mode and choosing MJPG — [Microsoft Q&A 5608812](https://learn.microsoft.com/en-us/answers/questions/5608812/how-to-avoid-ms-teams-to-select-mjpg-as-camera-dat)
  - The Teams desktop app using YUY2 at roughly 360p/480p in one user's tests — [Microsoft Q&A Anker C200](https://learn.microsoft.com/en-us/answers/questions/4444093/bad-quality-with-the-anker-c200-2k-camera?forum=msteams-all&page=2)
  - A virtual-camera project exposes 640x480/1280x720/1920x1080 YUY2 presets — [yushulx/virtual-camera](https://github.com/yushulx/virtual-camera)
  - Microsoft's UVC guide shows a typical camera offering YUY2 640x480@30 plus MJPG modes — [UVC implementation guide](https://learn.microsoft.com/en-us/windows-hardware/drivers/stream/uvc-camera-implementation-guide)
- **"Preview works, remote sees black" in general Teams reports (2020–2023, older material).**
  - Workarounds: start Teams before the virtual camera producer, rejoin the call, re-select the camera — [OBS forum: new Teams can't capture](https://obsproject.com/forum/threads/new-teams-cant-capture-the-virtual-camera.171102/), [Teams tech community thread](https://techcommunity.microsoft.com/discussions/microsoftteams/teams-users-webcam-is-showing-black-for-remote-participants/3772158), [FakeCam guide](https://www.fakecam.net/articles/virtual-camera-not-showing-in-teams)
  - A Microsoft Q&A answer reports Teams encodes camera video via Media Foundation, and suggests disabling hardware MFT encoders (registry `EnableEncoders`=0) when the preview works but remote doesn't. This is an anecdotal community fix, not official. — [Microsoft Q&A 5854110](https://learn.microsoft.com/en-us/answers/questions/5854110/on-desktop-teams-my-camera-and-screen-sharing-look)
- **New Teams uses Media Foundation.** New Teams and the Windows Camera app read MF cameras, so DirectShow-only filters don't appear. Page2Cam's MF source is the right technology. — relayed via search summary of [BlackCamera PR](https://github.com/Jop3r/BlackCamera/pull/1)

### Inferences
- **1. Media types (low cost, plausible Teams/Zoom fix).** Add 640x360, 640x480, 960x540 (and maybe 1280x720@15) entries, and YUY2 alongside NV12.
  - Order the list so NV12 1280x720 comes first. Many apps take the first or "best" type, and 1080p NV12 doubles the per-frame CPU conversion cost.
  - `p2c-shm` already scales, so this is mostly descriptor work.
  - Expected gain: compatibility with clients that negotiate lower modes, and less CPU when they do.
  - Caveat: speculative as a Teams-remote fix; nothing documents this cause.
- **2. Honor the D3D manager (medium cost).** When `SetD3DManager` gives an `IMFDXGIDeviceManager`:
  - open the device;
  - upload BGRA from shm into a D3D11 texture;
  - convert to NV12 with `ID3D11VideoProcessor`, or let the pipeline convert RGB32;
  - deliver via `MFCreateDXGISurfaceBuffer` from an allocator initialized with the manager (`IMFVideoSampleAllocatorEx::InitializeSampleAllocatorEx` with `MF_SA_D3D11_*` attributes).

  Expected gains and caveats:
  - Apps that pass a DXGI manager (the Camera app, possibly Teams' media stack) avoid a CPU→GPU upload on their side, and some apps' GPU encoders may behave better. One hypothesis for the Teams "local preview yes / remote no" symptom is a failing hardware-encoder path fed CPU samples; the `EnableEncoders` workaround points at the encoder. This is unproven.
  - Keep the CPU fallback, since Chrome/Edge/Teams ImageCapture do not pass a manager per VCamSample.
  - Risk: D3D device-removed handling inside Frame Server. Any crash takes down Frame Server for all cameras (cf. VCamSample #4), so this code must be very defensive.
- **3. Test protocol for the Teams remote issue.** Log the negotiated media type and whether a D3D manager was offered (`vcam.log` already exists).
  - Test new Teams with a second account to see what media type Teams' sending path selects.
  - Also test Teams web in Edge/Chrome, which likely uses Chromium's MF capture. If that works where desktop Teams doesn't, the problem is in Teams' native media stack.
- **4. Lifetime.** Keep `Session` + `CurrentUser`. It needs no admin, the camera disappears when Page2Cam exits, and stale "Page2Cam" entries can't confuse apps.
  - `System` would let Teams see the camera before Page2Cam starts, reducing the "start Teams first / re-select camera" problem. But the DLL would then serve a "waiting" pattern whenever the app isn't running, and orphaned devices are possible after uninstall (needs `Remove()` in the uninstall script).
  - Recommendation: Session by default, with System as an opt-in configured by the installer.
- **5. Friendly name.** It is set by the `friendlyName` argument, and Windows appends "Windows Virtual Camera". The fetched sources documented no supported icon API. (`IMFVirtualCamera::AddProperty` exists for device properties, but icon support isn't documented in what I read.)
- **6. Reliability.**
  - **Frame Server restarts.** Frame Server restarts when a source DLL crashes. The app's `IMFVirtualCamera` may then need `Stop`/`Start` or re-creation, so watch for errors and retry with backoff.
  - **Shared mapping.** The shm mapping is created by the DLL inside Frame Server, so a Frame Server restart recreates it. The app must reopen the mapping when its handle goes stale. Its current retry-each-second logic should cover the initial open, so also re-check periodically (for example, if the DLL heartbeat/seq doesn't move).
  - **Sleep/resume.** Register for `WM_POWERBROADCAST` / `PowerRegisterSuspendResumeNotification` and re-`Start()` the camera after resume.
  - None of this is documented by Microsoft for virtual cameras, so it is inferred from Frame Server behavior.
- **7. Refcounting.** Use `windows-rs` smart pointers only and never call `Release()` manually (lesson from VCamSample #4).

### Gaps
- No primary source explains why Teams remote participants don't receive VCamSample-style virtual cameras. The VCamSample author explicitly doesn't know.
- No source documents which media types new Teams/Zoom/Meet request from MF virtual cameras on Windows 11.
- No Microsoft documentation was found on Frame Server restart or sleep/resume handling for `MFCreateVirtualCamera` cameras.
- No documented icon registration for MF virtual cameras was found.

## Q4. Linux: v4l2loopback best practices and PipeWire alternatives (2025–2026)

### Takeaway
v4l2loopback with `exclusive_caps=1` remains the most compatible route for Chrome/Chromium-based clients (Meet, Teams web, Zoom web). YUYV (YUY2) is the safest format; it is what Page2Cam already writes.

Useful extras:
- `v4l2loopback-ctl set-fps` / `keep_format` / `sustain_framerate`;
- a `timeout` image so consumers don't freeze when the producer stalls;
- writing a frame before consumers open the device.

PipeWire virtual camera nodes are emerging but not reliable for browsers: the camera portal filters software nodes, browser PipeWire camera support is behind flags or distro defaults, and a portal UI is missing.

### Cited Findings
- **v4l2loopback README.**
  - With `exclusive_caps=1` the device "announces only OUTPUT capabilities" until a producer attaches, then only CAPTURE. If Chrome/WebRTC doesn't detect the device, try `exclusive_caps=1`.
  - Per-device controls: `keep_format`, `sustain_framerate` (duplicate frames to keep nominal fps), `timeout` (ms) with an optional timeout image.
  - `v4l2loopback-ctl add/delete/set-fps/set-caps` (e.g. `UYVY:640x480`); module options may be ignored for dynamically created devices.
  - Secure Boot needs module signing; DKMS doesn't install `v4l2loopback-ctl`.
  - Source: [umlaeute/v4l2loopback](https://github.com/umlaeute/v4l2loopback)
- **Formats (community, older).**
  - Chromium worked only with `exclusive_caps=1`, and YUY2 is supported by both Firefox and Chromium.
  - One report says Chrome only accepted yuv420p. MJPEG input worked when converted to YUYV before writing.
  - No source confirmed NV12 support in Chrome through v4l2loopback.
  - Sources: [v4l2loopback issue 78](https://github.com/umlaeute/v4l2loopback/issues/78), [StackOverflow: Chrome cannot detect v4l2loopback](https://kiwix.lema.org/stackoverflow.com_en_all_2023-11/questions/65514730/google-chrome-cannot-detect-v4l2loopback-device), [StackOverflow: MJPEG to yuyv422](https://kiwix.lema.org/stackoverflow.com_en_all_2023-11/questions/59574987/how-to-change-mjpeg-to-yuyv422-from-a-webcam-to-a-v4l2loopback)
- **max_buffers.** `max_buffers=2` appears in a user's setup with no rationale. — [NVIDIA forum](https://forums.developer.nvidia.com/t/v4l2camera-buffer-allocation-request-failed-with-error-invalid-argument/156363)
- **PipeWire and browsers.**
  - Firefox 116+ can use PipeWire cameras behind `media.webrtc.camera.allow-pipewire`; Fedora 41 made it the default.
  - Firefox 146 failed to find a v4l2loopback camera that Chromium and VLC found.
  - The camera portal "filters out anything but 'real' webcams and ignores purely software PipeWire nodes" (single Arch forum report).
  - Chromium's PipeWire camera support requires a setting.
  - A Mozilla bug notes the camera portal UI is still missing.
  - A Red Hat bug: Firefox with the libcamera backend cannot detect the OBS virtual camera.
  - Sources: [Mozilla bug 1672944](https://bugzilla.mozilla.org/show_bug.cgi?id=1672944), [Mozilla bug 2007675](https://bugzilla.mozilla.org/show_bug.cgi?id=2007675), [Arch BBS 314930](https://bbs.archlinux.org/viewtopic.php?id=314930), [Red Hat bug 2412269](https://bugzilla.redhat.com/show_bug.cgi?id=2412269), [scrcpy #4507](https://github.com/Genymobile/scrcpy/issues/4507)
- **PipeWire as a replacement (Ideas on Board).** They argue "PipeWire is the new v4l2loopback": custom source nodes as separate processes, showing a camera node in Firefox's selector. The article returned 403, so I relied on the search snippet. — [Ideas on Board](https://ideasonboard.com/news/pipewire-is-the-new-v4l2loopback)
- **In-kernel virtual camera driver.** An RFC v2 patch was posted to LKML in Feb 2026. It is a possible future alternative and still an RFC. — [LKML RFC](https://lkml.iu.edu/hypermail/linux/kernel/2602.0/04155.html)

### Inferences
- **Keep YUYV at 1280x720 or 1920x1080.** Write the first frame (black or "starting") immediately after `S_FMT` so `exclusive_caps` flips the device to CAPTURE before a browser enumerates it.
  - Set `keep_format=1` and `sustain_framerate=1` via `v4l2-ctl -d /dev/videoN -c ...`.
  - Set a `timeout` (e.g. 2000 ms) so consumers see a placeholder instead of a frozen frame if Page2Cam stalls. This mirrors the Windows DLL's "waiting" pattern.
- **Format choice.** 720p halves the YUYV conversion and write bandwidth, from about 4 MB to 1.8 MB per frame (2 bytes/px). Conferencing apps rarely send more than 720p, so 720p as the default camera output could be a real CPU saving. Expected gain is about 55% less conversion work; caveat: lower quality for screen-text pages.
- **Ship a modprobe config.** Use `/etc/modprobe.d/page2cam.conf` (`options v4l2loopback exclusive_caps=1 card_label=Page2Cam`) plus `/etc/modules-load.d/`. Document Secure Boot signing.
- **PipeWire.** Track it, but don't switch in 2026. A PipeWire node could be added later as an optional second output for Firefox/Fedora users, but the portal filtering of software nodes makes it unreliable for browsers today.

### Gaps
- No 2025–2026 primary source lists Chrome's accepted V4L2 pixel formats (YUYV vs NV12 vs MJPEG) for loopback devices.
- No authoritative PipeWire camera-portal statement on whether software nodes are intentionally filtered.
- The Ideas on Board article could not be fetched (403).
