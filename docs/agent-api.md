# Controlling Page2Cam from agents and scripts

Page2Cam can be driven by desktop AI agents over **MCP** (Model Context Protocol) and by scripts over its **local admin API**. Both listen on `127.0.0.1` only.

## Switching it on

Agents are **off by default**. Turn them on with **Allow agents** in the tray menu or in the control page's *Agents* panel. Page2Cam then:

- starts its MCP endpoint at `http://127.0.0.1:8766/mcp` (another free port when 8766 is taken),
- writes `%LOCALAPPDATA%\page2cam\agent.json` = `{ "url": "…/mcp", "token": "…" }` (deleted when switched off or on Quit),
- registers itself with the agents it finds (below).

Agents act **without asking**. Every change they make is listed in the *Agents* panel's activity list. The tray shows a notification when an agent starts or stops Page2Cam or your stream, changes the page or background while Page2Cam runs, or installs an update. The tray icon has a blue badge for a minute after an agent call.

**Reset agent access** makes a new token (clients using the URL need it again; the stdio entry reads it by itself).

## Setup per agent

Switching on registers `page2cam.exe --mcp` (stdio) with:

| Agent | Where | Entry |
|---|---|---|
| Claude Desktop | newest layout first: `%LOCALAPPDATA%\Claude-Data\claude_*\claude_desktop_config.json` (Claude Desktop 6+), the packaged app's `%LOCALAPPDATA%\Packages\Claude_*\LocalCache\Roaming\Claude\…`, then `%APPDATA%\Claude\…`. A new file starts from the newest older one (other servers stay); switching off also removes our entry from the older files | `mcpServers.page2cam = { "command": "<exe>", "args": ["--mcp"] }` — restart Claude Desktop afterwards |
| Claude Code | its CLI: `claude mcp add --scope user page2cam -- "<exe>" --mcp` | only when `claude` is on PATH |
| OpenAI Codex (app, CLI, IDE) | `%USERPROFILE%\.codex\config.toml` | `[mcp_servers.page2cam]` `command = "<exe>"`, `args = ["--mcp"]` (comments in the file are kept) |
| Cursor | `%USERPROFILE%\.cursor\mcp.json` | `mcpServers.page2cam` as Claude Desktop |
| VS Code (agent mode) | `%APPDATA%\Code\User\mcp.json` | `servers.page2cam = { "type": "stdio", "command": "<exe>", "args": ["--mcp"] }` |

Only agents whose config folder exists are touched; other entries are kept; a file Page2Cam can't read (e.g. JSON with comments) is left alone and the panel says so. Switching off removes the entries. **Uninstalling Page2Cam does not edit agent configs**: the entry then points to a missing program and the agent reports that it can't start it — remove it there.

Any other MCP client: use the stdio command `"<exe>" --mcp`, or the URL with the header `Authorization: Bearer <token from agent.json>` (Streamable HTTP, JSON answers, no SSE). The *Agents* panel shows both snippets.

`page2cam.exe --mcp` starts the tray (stopped) if it isn't running, detached from the agent, so closing the agent never closes Page2Cam. With agents off it still lists the tools and answers every call with "Turn on 'Allow agents' in the Page2Cam tray menu".

## Tools

Coordinates are 0..1 of the 1920×1080 output.

| Tool | What it does |
|---|---|
| `status` | state (stopped/starting/running/…), health, page, webcam, overlay, cut-out, background, overlay layers (`overlay_layers` visible, `overlay_on`), stream, audio, GPU, update; works while stopped |
| `start`, `stop`, `force_restart` | run control (start waits until running, up to ~40 s) |
| `open_url(url)`, `navigate(back\|forward\|reload)` | the page |
| `show_page(name, html)` | saves `Documents\Page2Cam\pages\<name>.html` (single file; overwrites) and shows it |
| `list_pages` | pages saved by `show_page` |
| `click(x, y)`, `scroll(x, y, dx, dy)`, `type_text(text)`, `press_key(key)` | page input |
| `snapshot` | the current output as a 1280×720 JPEG |
| `get_overlay`, `set_overlay(visible, x, y, width, style: box\|cutout_small\|cutout_full)` | webcam overlay (give only what changes) |
| `set_cutout_quality(quality\|fast)` | cut-out model |
| `get_crop`, `set_crop(x, y, w, h)`, `reset_crop` | webcam crop |
| `start_stream`, `stop_stream` | stream to the destination and key saved in the control page (agents can't pass or read a key) |
| `set_audio(mic_on, mic_gain, page_on, page_gain)` | stream sound (give only what changes) |
| `list_gpus`, `set_gpu(choice)` | video processing; restarts Page2Cam when needed |
| `install_update` | installs the update on offer |
| `get_background`, `set_background(full, full_colour, youtube, screen, in_box, box_colour, refined_edges)` | backgrounds: full frame `page\|image\|colour\|youtube\|screen\|blurred-room`, in box `room\|blur\|image\|colour`, refined edges (give only what changes); `screen` = a monitor id from `list_screens` (default: the main monitor) |
| `list_screens` | the monitors the screen background can show: `id`, `index`, `name`, `width`, `height`, `primary` (Windows; elsewhere an error) |
| `upload_background_image(row: full\|box, path \| base64)` | a PNG/JPEG/WebP picture (≤ 20 MB) for a background row, shown at once |
| `list_layers` | the overlay layers, back to front (HTML left out: `html_bytes` instead) |
| `add_layer(kind, …)` | a new overlay layer (see below); returns it with its `id` |
| `update_layer(id, …)` | change only the given fields (`null` clears `hide_after_s`, `colour`, `background`) |
| `show_layer(id)`, `hide_layer(id)` | with the enter / exit animation; showing restarts the auto-hide |
| `remove_layer(id)`, `clear_layers` | delete one (and its stored file) / hide all |
| `reorder_layers(ids)` | restack: back to front, the last in front; unlisted layers stay behind |

Tools that need the running engine answer "Page2Cam is stopped — call start first" while it is stopped. No tool returns the stream key or any token.

**Screen background.** `set_background(full: "screen")` shows a whole monitor, live, to the call or stream (everything on it: notifications, other windows, the pointer). The tray always shows a notification "Agent “…” is sharing your screen" for it, also while Page2Cam is stopped. While the screen shows, `snapshot` contains that monitor's picture. Choosing a page (`open_url`, the control page's address bar) or another background stops the capture. A monitor that can't be captured (unplugged) leaves a black background and says why in `status` (`background_note`).

## Overlay layers

Overlay layers are graphics drawn **over everything**, the person included: titles and lower thirds, a ticker along the bottom, any HTML (infographics, charts, small animations), pictures, videos and YouTube boxes. Page2Cam renders them in a second, transparent Chromium page and blends it over the output; while no layer is visible that page is paused and costs nothing.

Every layer has `x`, `y` (top-left corner) and `w`, `h` (size), all 0..1 of the 1920×1080 output; `z` (stacking, higher in front; a new layer goes in front), `visible`, `opacity` (0..1), `enter` / `exit` animations (`none`, `fade` (default), `slide-left`, `slide-right`, `slide-up`, `slide-down`), optional `hide_after_s` (1–3600: hides by itself), optional `name`, and an `id` (yours, sanitised to `a-z0-9-`, or made from the name / kind: `title-1`). Missing geometry gets the kind's default (title 0.05, 0.72, 0.5×0.16; ticker along the bottom; others top right). A layer shows at once unless `visible: false`.

| `kind` | Fields |
|---|---|
| `title` | `text`, `subtitle`, `style` (`news` \| `minimal` \| `brand`), `colour`, `background` (`#rrggbb`) |
| `ticker` | `items` (1–50 texts), `speed` (px/s at 1920 wide, default 120), `style`, `colour`, `background` |
| `html` | `html`: a fragment with its own `<style>` / `<script>`, ≤ 256 KB, on a transparent page |
| `image` | `path` (a local file, read once and copied) or `data_base64`; PNG, JPEG, WebP, GIF ≤ 20 MB; transparency kept |
| `video` | `path` (local MP4/WebM ≤ 200 MB, copied) or `url` (http/https); `loop`, `muted` (both default true) |
| `youtube` | `link` (a YouTube link or id), `start_s`, `loop`, `muted` (both default true) |

Styles: `news` (navy band with a red accent), `minimal` (white text with a shadow, no box), `brand` (Page2Cam navy and blue).

**Security.** An HTML layer runs in a sandboxed frame with an opaque origin (`sandbox="allow-scripts"`): it may load from the internet (live data), but it cannot reach the control API (no token, no same-origin reads), cookies, the top page or popups. Titles and tickers are drawn as text, never as HTML. Files given by `path` must be on a local drive (no network shares); Page2Cam copies them into its own folder and serves only those copies.

**Sound.** Unmuted videos and YouTube boxes play into the stream as part of page sound (same on/off and volume), never into calls. Layers start muted.

**Live changes.** Moving or restyling one layer doesn't restart the others (a ticker keeps scrolling, a video keeps playing). Layers are saved and come back **hidden** after a restart. The control page's *Overlays* panel lists every layer (show/hide, delete, drag and resize in the preview).

Examples (MCP `tools/call` arguments):

```json
{ "name": "add_layer", "arguments": {
  "kind": "title", "id": "guest", "text": "Ada Lovelace", "subtitle": "Chief Analyst",
  "style": "news", "enter": "slide-left", "exit": "fade", "hide_after_s": 12 } }
```

```json
{ "name": "add_layer", "arguments": {
  "kind": "ticker", "id": "news", "style": "brand", "speed": 140,
  "items": ["Q3 revenue up 12%", "New office opens in Lisbon", "Questions? Use the chat"] } }
```

```json
{ "name": "add_layer", "arguments": {
  "kind": "html", "id": "kpi", "x": 0.66, "y": 0.06, "w": 0.3, "h": 0.3, "enter": "fade",
  "html": "<style>body{font:600 28px Segoe UI,sans-serif;color:#fff} .card{background:rgba(13,27,42,.85);border-radius:16px;padding:18px;height:calc(100% - 36px)} .bar{height:22px;margin:10px 0;background:#0b63f0;border-radius:6px;animation:grow 1.2s ease-out both} @keyframes grow{from{width:0}}</style><div class=card>Sign-ups this week<div class=bar style=width:40%></div><div class=bar style=width:65%></div><div class=bar style=width:90%></div></div>" } }
```

Then `update_layer({ "id": "news", "items": ["Breaking: …"] })`, `hide_layer({ "id": "guest" })`, and check with `snapshot`.

## Local admin API (scripts)

`docs/openapi.json` describes the engine's routes. Every request needs:

- `Host: 127.0.0.1:<port>` (or `localhost:<port>`), and
- the per-launch token: header `X-P2C-Token: <token>` (or `?t=<token>`).

The port and token are in the control page's address (`http://127.0.0.1:<port>/?t=<token>`; the tray menu's *Open control page* opens it). They change on every start, so scripts that should survive restarts are better written against MCP.
