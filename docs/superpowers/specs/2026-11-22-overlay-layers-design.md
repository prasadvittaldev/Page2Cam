# Overlay layers — design

Status: draft for the user's approval (2026-11-22). Approach A chosen by the user.

## Intent

Broadcast-style graphics on top of the Page2Cam output, made and driven by AI agents during a call or stream: titles, a rolling ticker along the bottom, small animated infographics, and boxed clips from YouTube (or other video). Today the output has two layers (full-frame background, then the person as box or cut-out); overlays add a third, drawn **over everything**, including the person.

## Goals

- Layer kinds: **title / lower-third**, **ticker** (rolling text), **HTML** (any HTML/CSS/JS the agent writes: infographics, charts, small animations), **image** (PNG/JPEG/WebP/GIF, transparency kept), **video** (a local file or URL, looped or once), **YouTube** (a boxed clip).
- Every layer: position and size (0..1 of the 1920×1080 output), stacking order, visible on/off, opacity, optional enter/exit animation (fade / slide), optional auto-hide after N seconds.
- Agents create, change, show, hide, reorder and delete layers over MCP, and see the result (`snapshot` already shows the output).
- Changes appear live without restarting other layers (a ticker keeps scrolling, a video keeps playing while a title is added).
- The person running Page2Cam can see and control the layers in the control page.
- Costs nothing while no layer is visible.

## Non-goals (this round)

- A visual drag-and-drop layer editor (the control page gets a list with on/off, delete and drag in the preview, not a designer).
- Per-layer audio mixing controls (overlay sound follows the existing page-sound setting; see Audio).
- Overlays on the Linux build beyond what the CPU path gives for free (it should work, but only Windows is released).
- Hosting/sharing overlay templates online.

## Anti-goals

- Overlays must never make the camera/stream stutter more than today; a heavy overlay is throttled like a heavy page, never the webcam/output.
- Agent-written overlay code must not reach the control API, the stream key or the user's files.
- No overlay is shown that the user can't see listed and switch off.

## Approaches considered

- **A. One transparent overlay browser (chosen).** Page2Cam keeps a layer list and renders it as one transparent page in a second off-screen Chromium browser (same CEF instance), composited above the person. Every kind (text, HTML/JS, video, YouTube) works the same way; agents already write HTML. Costs one more Chromium page while something is visible.
- B. Native layers drawn by Page2Cam (text/images ourselves, video via FFmpeg). Cheapest for static images, but much new code (text layout, animation), and HTML/YouTube still need a browser, so it ends up containing A.
- C. Inject overlays into the background page. Breaks with screen sharing, blurred room and sites that block injected code, and the person would be drawn over the titles. Dropped.

## Design

### Layer model (engine, platform-free)

`Layer { id, name, kind, x, y, w, h, z, visible, opacity, enter: none|fade|slide-left|slide-up…, exit, hide_after_s: Option<u32>, content }` with `content` per kind:

- `title { text, subtitle?, style: preset name, colours?, font? }`
- `ticker { items: [text], speed (px/s), style }`
- `html { html }` (a full fragment with its own `<style>`/`<script>`; scoped as below)
- `image { file }` (uploaded/stored copy)
- `video { file | url, loop, muted }`
- `youtube { link, start_s?, loop, muted }`

IDs are short slugs (`title-1`, or agent-chosen names, sanitised, unique). Layers live in `Arc<RwLock<LayerSet>>` in `App`, persisted to `<data dir>/overlays/layers.json` (assets in `<data dir>/overlays/assets/`), so they survive restarts; layers are **hidden at start-up** (my call: nothing appears in a call by surprise after a restart).

Built-in **style presets** for title/lower-third/ticker (e.g. `news`, `minimal`, `brand` in Page2Cam's colours) so agents get good-looking results without writing CSS; `html` is the escape hatch for anything else.

### Rendering

- A second CEF browser, windowless, **transparent background** (`BrowserSettings.background_color` alpha 0), 1920×1080, `windowless_frame_rate` from config, same GPU adapter and shared-texture path as the page (CPU paints on the CPU path), its own `FrameSlot`/texture input.
- It loads `http://127.0.0.1:<port>/ov/<per-launch id>/scene.html`: a host page (served by the admin server, like `/bg/`) that holds one absolutely positioned container per layer. Each `html` layer runs inside a **sandboxed `<iframe srcdoc>`** (`sandbox="allow-scripts"`, no same-origin), so its script can't reach the host page or our origin; title/ticker/image/video are rendered by the host page's own trusted code from the layer data; YouTube uses the sandboxed youtube-nocookie embed already used for backgrounds.
- Live updates: the host page polls (or keeps an SSE/WebSocket to) `/ov/<id>/state` and applies diffs (add/remove/move/show/hide), so untouched layers keep running. My call: WebSocket on the existing admin server, falling back to 500 ms polling.
- Idle: when no layer is visible the overlay browser's paints are paused (existing pause flag) and its frame rate set to 1; the compositor skips the overlay pass. Showing a layer resumes it and the first composited frame waits for its first paint (≤ 1 tick of lag).
- The page frame-rate throttle (`PageFps`) applies to the overlay browser too (same rate as the page).

### Compositing

- A new pass after the person/PIP: alpha-blend the overlay frame (CEF gives premultiplied BGRA) over the output, on the GPU path (new `OverlayInput` like `PageInput`, one shader pass) and the CPU path (`compose()` gets an optional overlay frame). Output stays opaque.
- `RedrawWhy` gains `overlay`; the 30 s output line counts it.

### Audio

Overlay sound (video/YouTube, unmuted) goes to the stream only, mixed as part of **page sound** (same on/off and gain), never into calls. Layers default to muted unless the agent/user unmutes them (my call: a surprise sound in a stream is worse than a silent clip).

### Agents (MCP)

New tools (all need the engine running, like the others):

- `list_layers` — id, name, kind, position/size, z, visible, a short content summary.
- `add_layer(kind, …content, x, y, w, h, z?, visible?, enter?, exit?, hide_after_s?, name?)` → id. `html` content up to 256 KB; `image`/`video` via `path` or base64 (≤ 20 MB image, ≤ 200 MB video by path only).
- `update_layer(id, …any field)` — partial, like `set_overlay`.
- `show_layer(id)`, `hide_layer(id)` (play enter/exit animation), `remove_layer(id)`, `clear_layers` (hide all).
- `reorder_layers(ids)` or `z` on update.
- Presets listed in the tool descriptions so agents pick them without guessing.

Tray balloons: none for layer changes (they're frequent); the Agents panel activity list records every change, as for other tools. My call — revisit if the user wants a heads-up.

### Control page

An **Overlays** panel: the layer list (name, kind, visible toggle, delete), quick-add for Title and Ticker (text box + preset), selected layer outlined in the preview and draggable/resizable there (same 0..1 geometry the PIP uses), and a "Hide all" button.

### Admin API

`GET/POST /api/layers`, `POST /api/layers/<id>` (partial), `DELETE /api/layers/<id>`, `POST /api/layers/<id>/asset` (upload), all token + Host checked; the scene/state routes under `/ov/<per-launch id>/` skip the token like `/bg/` (unguessable id, Host checked, CSP: scripts only in sandboxed frames + the host page's own script, media from our origin, YouTube embed, and the URLs given in `video.url`).

### Security

- Agent HTML runs only inside sandboxed opaque-origin iframes: no access to the admin API (token never present, Host check stays), no cookies, no top navigation, no popups. It may fetch from the internet (infographics with live data); my call, since agents can already open any web page as the background.
- Stored assets are copied into Page2Cam's data folder; paths from agents are read once at upload, never served directly.

## Constraints

- Windows 11 GPU path must keep the output at 30 fps on the user's laptop with one title + ticker visible; overlay pass budget ≤ 1 ms GPU (NVIDIA) like other passes.
- CEF can initialise only once per process: the overlay browser is a second browser in the same CEF instance, pumped by the same main-thread loop.
- GPL-3.0; no new external runtime dependencies.

## Left to the builder

- Exact preset CSS and animation timings.
- WebSocket message format for scene diffs.
- How `BrowserHandle`/`run` grow to host two browsers (a second handle vs. a browser id on commands).
- Test strategy details (platform-free layer model, scene diffing and MCP schemas unit-tested; real CEF transparency checked in the render test; GPU blend checked in `p2c-d3d` tests).
