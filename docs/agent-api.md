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
| Claude Desktop | `%APPDATA%\Claude\claude_desktop_config.json` | `mcpServers.page2cam = { "command": "<exe>", "args": ["--mcp"] }` — restart Claude Desktop afterwards |
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
| `status` | state (stopped/starting/running/…), health, page, webcam, overlay, cut-out, background, stream, audio, GPU, update; works while stopped |
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
| `get_background`, `set_background(full, full_colour, youtube, in_box, box_colour, refined_edges)` | backgrounds: full frame `page\|image\|colour\|youtube\|blurred-room`, in box `room\|blur\|image\|colour`, refined edges (give only what changes) |
| `upload_background_image(row: full\|box, path \| base64)` | a PNG/JPEG/WebP picture (≤ 20 MB) for a background row, shown at once |

Tools that need the running engine answer "Page2Cam is stopped — call start first" while it is stopped. No tool returns the stream key or any token.

## Local admin API (scripts)

`docs/openapi.json` describes the engine's routes. Every request needs:

- `Host: 127.0.0.1:<port>` (or `localhost:<port>`), and
- the per-launch token: header `X-P2C-Token: <token>` (or `?t=<token>`).

The port and token are in the control page's address (`http://127.0.0.1:<port>/?t=<token>`; the tray menu's *Open control page* opens it). They change on every start, so scripts that should survive restarts are better written against MCP.
