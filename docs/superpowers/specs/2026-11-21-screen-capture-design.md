# Screen background — design (2026-11-21)

**Request (user):** "add the capability of streaming a screen … make sure this is available in MCP for agents". Decided by the implementer (the user asked to just start); calls marked *call*.

## What it is

A new full-frame background `screen`: one monitor, live, fills the output where the page would be; the webcam (cut-out or box) is drawn over it exactly as over a page. Works for the virtual camera, the stream, the preview and `snapshot`.

Goals: one switch in the control page and one MCP call; cheap when the screen is still; safe defaults (main monitor). Non-goals: window/region capture, Linux capture, picking a monitor from the tray menu. Anti-goal: an agent silently showing the user's screen.

## Approaches considered

1. **Captured frames replace the page (chosen).** Treat `screen` as a generated background (Chromium shows a black page, so all existing page/background interplay — last page URL, Back/Forward, Box→cut-out rules, "navigate returns to Page" — applies unchanged), drop Chromium's paints while it shows, and feed captured frames into the same page inputs the compositors already draw: the CPU `FrameSlot` and, on the GPU path, the `PageInput` texture (new `upload_frame`). No compositor change; letterboxing and scaling come for free.
2. A new compositor backdrop `FullBackdrop::Screen` with its own texture in both compositors. Cleaner separation, but touches both compositors, the change detection and the GPU tests — much more code for the same picture.
3. Capture into Chromium (e.g. a `getDisplayMedia` page). Needs a user gesture/permission prompt in an offscreen browser and costs Chromium GPU time. Rejected.

## Decisions

- *call* **Capture API:** Windows Graphics Capture (`CreateForMonitor`), free-threaded frame pool on its own D3D11 device (default hardware adapter), polled by a thread at most `fps` times a second; only changed frames arrive, so a still screen costs nothing. Frames are read back through a staging texture to BGRA (alpha forced opaque) and handed to the sink; the GPU path uploads them again (`UpdateSubresource`). Two copies per changed frame were judged acceptable for v1 (see Open).
- *call* **Pause, not navigate away:** `BrowserConfig.paused` (shared `AtomicBool`, settable before the browser exists, so a saved screen background never flashes the page at start-up). Turning on: pause first, then start the capture. Turning off: stop the capture first, then un-pause and repaint.
- *call* **Monitor ids:** the display device name without `\\.\` (`DISPLAY1`); requests may also give a 1-based index, or empty/`primary`. The resolved id is saved (`background_screen`). Sizes are physical (`EnumDisplaySettingsW`), not DPI-scaled.
- *call* **Cursor on**, **yellow capture border left on** (Windows draws it; a visible sign that the screen is shared).
- **Errors:** an unknown monitor is refused (400, nothing changed); a start failure (monitor unplugged since saving) keeps the `screen` choice, shows black, and the reason is the first `background_note`. Linux: `GET /api/screens` → 501 `{error: "screen capture is Windows-only for now"}`, `set_background(screen)` → 400 with the same text; the tile is disabled with that tooltip.
- **Admin:** `GET /api/screens` (`ScreenDto`), `BackgroundDto.full_screen`, a "Screen" tile with a monitor picker and the warning "Everything on this monitor is shown to your call or stream, live …".
- **MCP:** tool 29 `list_screens`; `set_background` gains `full: "screen"` and `screen: <id>`; tray balloon "Agent “X” is sharing your screen" for every such call, also while stopped (stronger than other background balloons, which only show while running).

## Open (for the user)

- Cost on large monitors: a 4K screen is ~33 MB per changed frame, copied twice; fine for slides, heavy for video playback. A GPU-only path (capture on the compositor's device, `CopyResource` into the page texture) would remove both copies.
- Window capture (one app instead of a whole monitor) and hiding the yellow border are not done.
- The capture always runs on the default hardware adapter, not the one chosen under "Video processing".
