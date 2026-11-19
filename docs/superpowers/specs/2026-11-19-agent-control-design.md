# Agent control (MCP) — design

Date: 2026-11-19. Status: awaiting user approval (builder check done).

## Why
The user wants Claude Desktop and other desktop agents to operate Page2Cam "in any situation": run it, create good backgrounds (the background is a web page, so an agent can write one), and use it for purposes not yet named. The agent should be able to take control of **all settings and features**, and the feature should be visible and switchable **in the tray and the admin page**.

## Goals
- Any MCP-capable desktop agent (Claude Desktop, Claude Code, OpenAI Codex (desktop app / CLI / IDE extension), Cursor, Windsurf, VS Code Copilot agent mode, Cline, Continue, Zed, Goose, LM Studio) can control Page2Cam.
- Full control: everything the admin page can do, plus Start / Stop / Force restart, writing and showing its own pages, and seeing what the camera sends (snapshot).
- Works from a cold start: "start Page2Cam and show my agenda" works while Page2Cam is stopped (the tray is running).
- The user is **informed, not asked** (user's decision: "just intimation is good enough"): no confirmation prompts; notifications and an activity list instead.
- One switch, **Allow agents**, in the tray menu and the admin page; off by default.

## Non-goals
- Agents on other machines or reaching Page2Cam over the internet (ChatGPT desktop's remote-only connectors are out of scope).
- Confirmation prompts / per-action approval.
- macOS. Linux gets the stdio helper against a running standalone app only (no tray on Linux) — builder may defer Linux entirely if it costs more than a small adapter.
- Changing what the engine can do; agent tools map onto existing features (plus snapshot and show-page). Features from the coming quality round (refined edges, background modes, YouTube background) get tools when they land.

## Anti-goals
- An agent learning secrets: the stream key, the admin token and the agent token are never returned by any tool or written to agent-visible output.
- The user not knowing an agent changed what a live call sees.
- Anything reachable from outside the PC (listen on 127.0.0.1 only; Host-header check as the admin server does).
- Breaking the tray's < 1 % CPU / < 40 MB idle targets.

## Approaches (user picked A + local address)
- **A (chosen): built into Page2Cam, stdio.** `page2cam.exe --mcp` is started by the agent and speaks MCP over stdin/stdout. Supported by every local MCP client; can start Page2Cam from cold.
- **B (chosen, alongside A): local address.** The tray serves MCP over Streamable HTTP at `http://127.0.0.1:<port>/mcp` for clients that prefer a URL. Tools are written once and shared by A and B.
- C (rejected for now): separate `.mcpb` Claude Desktop extension in JavaScript — a second codebase to keep in step, Claude Desktop-centric. May later wrap A as a `.mcpb` for one-click install, without new code.
- Also: the existing local admin API is documented (OpenAPI file) for scripts and non-MCP agents.

## Design

### Decisions settled after the builder check (designer's calls)
1. **Tray control endpoint, always on.** The tray runs one small HTTP server on 127.0.0.1 from start: `/tray/*` control routes (agents on/off, reset agent access, activity list, registered agents, install update) are always served, guarded by a tray token the tray passes to the engine in the environment (`P2C_TRAY_URL`, `P2C_TRAY_TOKEN`, token never on the command line); `/mcp` is served only while agents are allowed (404 otherwise). The engine proxies the admin page's Agents panel to `/tray/*` and reports `agents_allowed` in `/api/state`. An idle listener costs nothing measurable, so the tray targets hold.
2. **New engine input route** `POST /api/input` taking the existing `InputEvent` JSON with output coordinates 0..1 (same mapping the WebSocket uses after `map_preview`), guarded like every route. The tray's click/scroll/type/key tools use it; no WebSocket client in the tray.
3. **Bridge start-up:** `--mcp` reads `tray.toml` first. Agents off → no tray launch; every call returns "Turn on 'Allow agents' in the Page2Cam tray menu". Agents on and no tray → launch the tray **detached from the agent's process tree** (`DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB | CREATE_NEW_PROCESS_GROUP`), so closing the agent never takes the tray or the camera with it; wait ≤ 10 s for the discovery file.
4. **Who handles what:** `start` / `stop` / `force_restart` / `install_update` / `status`-while-stopped → tray. `set_gpu` → engine; when a restart is needed it uses the existing restart path (engine exits 75, the tray restarts it). `stop` while streaming just stops (the engine's shutdown ends the stream) and the balloon says "stream ended".
5. **Registration per agent:** Claude Code via its own CLI (`claude mcp add --scope user page2cam -- "<exe>" --mcp` / `claude mcp remove`) when `claude` is on PATH — never edit `~/.claude.json` directly (Claude Code rewrites it while running); otherwise the admin page shows the snippet. Other configs are edited atomically (write temp + rename), JSON with order kept, Codex TOML with `toml_edit`. Registrations stay on Quit (the bridge starts the tray on demand). The uninstaller does not edit agent configs (it runs elevated as possibly another user); after uninstall the entry fails to launch and the agent shows that — documented in README.
6. **Snapshot:** new engine route `GET /api/snapshot` encodes the **latest output frame** on demand (not the preview stream), scaled to 1280×720, JPEG quality 80; target < 50 ms on the user's laptop. No always-on encoder.
7. **show_page:** single-file HTML (agents inline images as data: URIs or link http(s) assets); a name that exists is overwritten; names are sanitised to `[A-Za-z0-9 _-]{1,64}` (anything else → `_`, Windows reserved names prefixed) and always end in `.html`; folder = the Documents known folder (`FOLDERID_Documents`, follows OneDrive/localised redirection) + `Page2Cam\pages`.
8. **"Allow agents"** lives in `tray.toml` (`agents_allowed`, `agent_token`); the engine reads it only through the tray endpoint.

### Shape
```
agent ──stdio──▶ page2cam.exe --mcp ──HTTP──▶ tray /mcp ──admin API──▶ engine
agent ──Streamable HTTP──────────────────────▶ tray /mcp
```
- **The tray hosts the MCP server** (Windows). All tools live in one module in `p2c-tray` (or a new `p2c-mcp` crate, builder's call). The tray already holds the engine's port and admin token and owns Start/Stop/Force restart.
- **`--mcp` is a thin stdio↔HTTP bridge.** It reads the tray's discovery file, forwards JSON-RPC messages, and passes the agent's client name. If no tray is running it launches the tray (stopped) and waits up to 10 s. If agents are off it answers every call with an error telling the user to switch on "Allow agents" in the tray.
- **Discovery file:** `%LOCALAPPDATA%\page2cam\agent.json` = `{ "url": "http://127.0.0.1:<port>/mcp", "token": "<agent token>" }`, written by the tray while agents are allowed, deleted when switched off or on Quit. LocalAppData is per-user, so only the user's own processes can read it — same trust level as the existing per-launch admin token.
- **Agent token** is separate from the engine's admin token (header `Authorization: Bearer <token>`), generated once and kept in `tray.toml`; "Reset agent access" in the admin page rotates it (and rewrites registrations). Requests also need `Host: 127.0.0.1:<port>`.
- **Port:** fixed default 8766 (so URL-configured clients keep working across launches), falling back to a free port and rewriting the discovery file and registrations if it is taken.
- **Tool calls while stopped:** tools that need the engine return a clear error ("Page2Cam is stopped — call start first") instead of auto-starting; `status`, `start`, `force_restart`, `list_pages` work while stopped.

### Tools (first set)
All coordinates on the output are 0..1 of the 1920×1080 frame. Tool descriptions tell the agent about the canvas (1920×1080, where the webcam overlay / cut-out person stands) so generated pages leave room for the person.
- **Run:** `status` (state, health level and reasons, current page, webcam, overlay, cut-out, stream, audio, GPU, update — no secrets), `start`, `stop`, `force_restart`.
- **Page:** `open_url(url)` (http/https/file, as `normalize_url`), `navigate(back|forward|reload)`, `show_page(html, name)` — saves to `Documents\Page2Cam\pages\<name>.html` and shows it; `list_pages`; `click(x, y)`, `scroll(x, y, dx, dy)`, `type_text(text)`, `press_key(key)`.
- **See:** `snapshot()` — the current output frame as a JPEG image (≤ 1280 wide), so the agent can check its work. Needs a new engine route `GET /api/snapshot` (latest preview JPEG).
- **Webcam:** `get_overlay` / `set_overlay(visible, x, y, width, style: box|cutout_small|cutout_full)`, `set_cutout_quality(quality|fast)`, `get_crop` / `set_crop(...)` / `reset_crop`.
- **Stream:** `start_stream()` to the saved destination with the saved key (the agent can't pass or read a key; if none is saved the error says to save one in the admin page), `stop_stream`.
- **Audio:** `set_audio(mic_on, mic_gain, page_on, page_gain)`.
- **Setup:** `list_gpus`, `set_gpu(choice)` (+ restart when needed), `install_update`.

### Informing the user
- Every tool call that changes something is recorded in an **activity list** in the tray (last 200 entries: time, agent name from the MCP client info, action, short result). The admin page shows it in an "Agents" panel (via an engine route that proxies to the tray; the tray passes its local address and token to the engine in the environment, like `P2C_ADMIN_TOKEN`).
- **Tray balloon** for actions that are public or leave the PC: stream started/stopped, Page2Cam started/stopped/restarted, and page changed while running — e.g. "Claude started your YouTube stream". Balloons are merged by the existing queue.
- **Tray icon marker** (small badge) while an agent made a call in the last 60 s; tooltip line "Agent: Claude Desktop (active)".

### Switching on and registration
- **Allow agents** (tray menu checkbox, admin page switch; stored in `tray.toml`; default off). Turning it on: start the MCP server, write the discovery file, and **register Page2Cam with installed agents** — add an `mcpServers.page2cam = { command: "<exe>", args: ["--mcp"] }` entry to Claude Desktop (`%APPDATA%\Claude\claude_desktop_config.json`), Claude Code (`~/.claude.json` user scope), OpenAI Codex (`~/.codex/config.toml`, a `[mcp_servers.page2cam]` table with `command`/`args`; TOML edited with `toml_edit` to keep comments), Cursor (`~/.cursor/mcp.json`) and VS Code (user `mcp.json`) when their config dir exists; preserve all other content; never touch a file we can't parse. Turning it off: stop the server, delete the discovery file, remove our entries.
- The admin page "Agents" panel shows which agents were registered and copy-paste snippets (stdio and URL+header) for any other client.
- A note that Claude Desktop must be restarted to pick up a new server.

### Local API documentation
- `docs/agent-api.md` + `docs/openapi.json` describing the engine's admin routes (with the token/Host rules) for non-MCP automation.

## Testing
- Unit: tool argument validation and mapping to admin calls (fake engine), secret redaction (no tool output contains the stream key or tokens — property test over all tools), discovery file write/delete, config registration add/remove preserving other entries and refusing unparseable files, port fallback, activity list ring and balloon selection.
- Protocol: MCP `initialize` / `tools/list` / `tools/call` round trip over stdio and over HTTP against an in-process tray server; MCP Inspector run manually.
- Acceptance with the user: Claude Desktop starts Page2Cam from stopped, writes and shows a background, takes a snapshot, moves the overlay, starts/stops a test stream — with balloons and the activity list showing each step.

## Left to the builder
- MCP implementation: the official Rust SDK (`rmcp`) if it builds in the workspace (including `cargo xwin`), else hand-written JSON-RPC (protocol version 2025-06-18 or the newest the target clients accept).
- Crate placement, exact tool names/argument schemas within the list above, snapshot JPEG quality, badge drawing, VS Code config path details, Linux adapter scope.
