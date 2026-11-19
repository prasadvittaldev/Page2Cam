# Agent Control (MCP) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Desktop agents (Claude Desktop, Claude Code, Codex, Cursor, VS Code, …) control all of Page2Cam over MCP — via `page2cam.exe --mcp` (stdio) or the tray's local address — with the user informed by balloons and an activity list, switched by "Allow agents" in the tray and admin page.

**Architecture:** A platform-free `p2c-mcp` crate holds the MCP protocol (hand-written JSON-RPC) and the tool set over a `Backend` trait. The tray (Windows) runs one small std-thread HTTP server: `/tray/*` control routes always, `/mcp` only while agents are allowed; its `Backend` calls the engine's admin API or the tray controller. `--mcp` is a stdio↔HTTP bridge that starts the tray detached when needed. The engine gains `/api/input`, `/api/snapshot` and an Agents proxy to the tray.

**Tech Stack:** Rust, serde_json, toml_edit, std::net (no tokio in the tray), axum (engine, existing), `windows` 0.62.

**Spec:** `docs/superpowers/specs/2026-11-19-agent-control-design.md` (approved 2026-11-19).

**Prerequisite:** the tray plan (`docs/superpowers/plans/2026-11-19-tray-app.md`) is complete.

## Global Constraints

- Listen on 127.0.0.1 only; every request checks `Host: 127.0.0.1:<port>` (or `localhost:<port>`) and a token. Engine routes keep `X-P2C-Token`; tray routes use `Authorization: Bearer <token>`.
- Secrets never leave: no tool result, error, log line or activity entry contains the stream key, the admin token, the tray token or the agent token.
- Tray endpoint: `/tray/*` always on (tray token, passed to the engine in env `P2C_TRAY_URL`, `P2C_TRAY_TOKEN`, never on the command line); `/mcp` only while agents are allowed (404 otherwise), agent token.
- Default agent port 8766; if taken, a free port; the discovery file and registrations follow the port.
- Discovery file `%LOCALAPPDATA%\page2cam\agent.json` = `{ "url": "http://127.0.0.1:<port>/mcp", "token": "<agent token>" }`, present only while agents are allowed and the tray runs.
- `tray.toml` keys: `agents_allowed` (default false), `agent_token` (generated once; rotated by "Reset agent access").
- MCP: JSON-RPC 2.0; methods `initialize`, `notifications/initialized`, `ping`, `tools/list`, `tools/call`; protocol version `2025-06-18` (also accept `2025-03-26` and answer with the client's if supported); HTTP transport answers POST with `application/json` (no SSE), GET `/mcp` → 405.
- Tool errors are tool results with `isError: true` and a plain-English message (e.g. "Page2Cam is stopped — call start first"; "Turn on 'Allow agents' in the Page2Cam tray menu").
- Coordinates in tools are 0..1 of the 1920×1080 output.
- Pages: `<Documents known folder>\Page2Cam\pages\<name>.html`; names sanitised to `[A-Za-z0-9 _-]{1,64}` (others → `_`; Windows reserved names prefixed `_`); existing name overwritten; single-file HTML.
- Snapshot: latest output frame, 1280×720, JPEG quality 80, on demand.
- Balloons (via the tray's existing queue) for: stream started/stopped, Page2Cam started/stopped/restarted (by an agent), page changed while running. Text form: "<Agent> started your YouTube stream".
- Activity list: last 200 entries `{ time, agent, action, result }` for every tool call that changes something.
- Registration: Claude Desktop `%APPDATA%\Claude\claude_desktop_config.json`, Codex `~/.codex/config.toml` (`[mcp_servers.page2cam]`), Cursor `~/.cursor/mcp.json`, VS Code `%APPDATA%\Code\User\mcp.json` (`servers` key) — only when the agent's config dir exists; atomic write (temp + rename); keep all other content; never touch an unparseable file. Claude Code only via `claude mcp add --scope user page2cam -- "<exe>" --mcp` / `claude mcp remove --scope user page2cam` when `claude` is on PATH; never edit `~/.claude.json`.
- Bridge: reads `tray.toml` first; agents off → no tray launch, every call answered with the "Allow agents" error; no tray running → launch it with `DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB | CREATE_NEW_PROCESS_GROUP`, wait ≤ 10 s for the discovery file.
- Tray targets stay: < 1 % CPU, < 40 MB RAM while stopped (the server thread blocks in `accept`).
- Linux: deferred (spec allows it) — `--mcp` on Linux prints "agent control is Windows-only for now" as an MCP error; p2c-mcp stays platform-free so a Linux backend can follow.
- Workspace builds on Linux and in `cargo xwin`; Windows-only code `cfg(windows)`. Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Don't run app instances while the user's Page2Cam runs.

## Review Focus

1. **Agent sends malformed or hostile arguments** (x = 5, huge html, name `..\..\x`, unknown style) — a tool error, never a panic, a write outside the pages folder, or a stuck engine. Test in Task 2 (`bad_arguments_are_tool_errors`) and Task 3 (`page_names_cannot_escape_folder`).
2. **Engine stopped / starting / crashed mid-call** — engine tools answer "stopped — call start first" or a timeout error within 5 s; the bridge never hangs the agent. Test in Task 4 (`engine_tools_when_stopped_fail_fast`).
3. **Agent config files the user has customised** (comments in Codex TOML, other servers, an existing `page2cam` entry, invalid JSON) — kept intact, invalid ones skipped and reported. Test in Task 3 (`registration_preserves_other_entries`, `registration_skips_unparseable`).
4. **Agents switched off while an agent is connected** — next call gets the "Allow agents" error; the discovery file is gone; registrations removed. Test in Task 4 (`switch_off_closes_mcp_and_cleans_up`).
5. **Secret leak through status or errors** (stream URL with key, ffmpeg error text, token in URL) — redacted. Test in Task 2 (`no_tool_output_contains_secrets`).

---

### Task 1: Engine routes for agents

**Files:** `crates/p2c-admin/src/{api.rs,server.rs}`, `crates/page2cam/src/app.rs`, `crates/p2c-admin/ui/index.html` (none yet — UI is Task 6)

**Interfaces:**
- Produces: `POST /api/input` body `{ "kind": "click"|"move"|"scroll"|"key"|"text", "x": f32, "y": f32, "dx"?: f32, "dy"?: f32, "key"?: String, "text"?: String }` (x/y output 0..1) → mapped to page px with the same letterbox mapping as `map_preview`, then `Controls::input`; `GET /api/snapshot` → `image/jpeg` (1280×720, q80) via new `Controls::snapshot(&self) -> Option<Vec<u8>>` (503 when no frame yet); `GET /api/agents` and `POST /api/agents` (`{ "allowed": bool }` | `{ "reset_token": true }`) proxied to `P2C_TRAY_URL` + `/tray/agents` with `P2C_TRAY_TOKEN` (503 "tray not available" in standalone); `Status.agents_allowed: Option<bool>` (`#[serde(default)]`, None in standalone).
- [ ] **Step 1: Failing tests:** `input_route_maps_output_coords_to_page_px` (fake Controls records events; 0.5,0.5 → page centre for a letterboxed page), `input_route_rejects_out_of_range` (x = 1.5 → 400), `snapshot_route_returns_jpeg_or_503`, `agents_route_without_tray_is_503`, `new_routes_require_token_and_host`.
- [ ] **Step 2–4:** run → fail; implement (`snapshot` in app.rs scales the latest output frame and JPEG-encodes with the existing `image` crate); `cargo test --workspace`, clippy, fmt.
- [ ] **Step 5: Commit** `feat(admin): input, snapshot and agents routes for agent control`.

### Task 2: `p2c-mcp` crate — protocol and tools

**Files:** Create `crates/p2c-mcp/{Cargo.toml,src/lib.rs,src/protocol.rs,src/tools.rs,src/redact.rs}`; add to workspace.

**Interfaces:**
- Produces: `trait Backend { fn call(&self, req: BackendCall) -> Result<BackendReply, String>; }` with `enum BackendCall { Status, Start, Stop, ForceRestart, OpenUrl(String), Navigate(Nav), ShowPage { name, html }, ListPages, Input(InputReq), Snapshot, GetOverlay, SetOverlay(OverlayReq), SetCutoutQuality(String), GetCrop, SetCrop(CropReq), ResetCrop, StartStream, StopStream, SetAudio(AudioReq), ListGpus, SetGpu(String), InstallUpdate }` and `enum BackendReply { Json(serde_json::Value), Image(Vec<u8>), Done(String) }`; `struct Server<B: Backend> { fn handle(&self, msg: &serde_json::Value, client: &mut ClientInfo) -> Option<serde_json::Value> }` (None for notifications); `fn redact(s: &str) -> String` (removes RTMP keys, `?t=`/token values, `Bearer …`).
- Tool names (exact): `status, start, stop, force_restart, open_url, navigate, show_page, list_pages, click, scroll, type_text, press_key, snapshot, get_overlay, set_overlay, set_cutout_quality, get_crop, set_crop, reset_crop, start_stream, stop_stream, set_audio, list_gpus, set_gpu, install_update`. Each has a JSON Schema and a description; `show_page`, `set_overlay` and `snapshot` descriptions explain the 1920×1080 canvas and where the person stands for each overlay style (box / cutout_small / cutout_full), and that pages should leave that area clear.
- `ClientInfo { name: String }` from `initialize.params.clientInfo.name` (default "Agent").
- [ ] **Step 1: Failing tests:** `initialize_negotiates_version` (2025-06-18 and 2025-03-26 echoed; unknown → 2025-06-18), `tools_list_has_all_tools_with_schemas` (exact name set above), `tools_call_maps_to_backend` (table: each tool + args → expected `BackendCall`), `bad_arguments_are_tool_errors` (x = 5, missing html, html > 2 MB, unknown style → `isError: true`, backend not called), `snapshot_returns_image_content` (base64 `image/jpeg`), `no_tool_output_contains_secrets` (backend replies/errors containing `rtmp://a.rtmp.youtube.com/live2/abcd-1234`, `?t=SECRET`, `Bearer SECRET` → output lacks `abcd-1234`/`SECRET`), `notification_has_no_reply`, `unknown_method_is_jsonrpc_error_-32601`.
- [ ] **Step 2–4:** run → fail; implement; tests, clippy (incl. Linux target), fmt.
- [ ] **Step 5: Commit** `feat(mcp): protocol and tool set over a backend trait`.

### Task 3: Agent settings, pages, registration, activity (platform-free)

**Files:** `crates/p2c-tray/src/{settings.rs,agents.rs}` (new `agents.rs`; pure functions + thin fs wrappers)

**Interfaces:**
- Produces: `agents_allowed_in(text) -> bool`, `with_agents_allowed(text, bool) -> String`, `agent_token_in(text) -> Option<String>`, `with_agent_token(text, &str) -> String` (toml_edit, comments kept); `fn sanitize_page_name(&str) -> String`; `fn page_path(dir: &Path, name: &str) -> PathBuf`; `fn write_discovery(path, url, token) -> io::Result<()>` / `remove_discovery(path)`; `enum AgentApp { ClaudeDesktop, Codex, Cursor, VsCode }` with `fn register_json(text: &str, key: &str, exe: &str) -> Result<String, RegError>` / `unregister_json(..)` (key `mcpServers` or `servers`), `register_codex_toml` / `unregister_codex_toml`, `fn registration_targets(env) -> Vec<(AgentApp, PathBuf)>` (only existing dirs), `fn apply_registration(on: bool, exe) -> Vec<(String, Result<(), String>)>` (atomic write; Claude Code via `claude mcp add/remove` when on PATH); `struct Activity` (ring of 200, `push`, `list`), `fn balloon_for(agent: &str, call: &BackendCall, running: bool) -> Option<String>`.
- [ ] **Step 1: Failing tests:** `agents_settings_roundtrip_keep_comments`, `page_names_cannot_escape_folder` (`..\..\x`, `CON`, `a/b`, empty, 200 chars, unicode → inside dir, `.html`, ≤ 64 chars + ext), `registration_preserves_other_entries` (JSON with other servers + existing page2cam entry replaced; Codex TOML with comments), `registration_skips_unparseable` (invalid JSON untouched, error returned), `unregister_removes_only_ours`, `activity_keeps_last_200`, `balloon_table` (stream start/stop, start/stop/restart, open_url while running → text; while stopped / get_* → None).
- [ ] **Step 2–4:** run → fail; implement; tests, clippy (incl. Linux target), fmt.
- [ ] **Step 5: Commit** `feat(tray): agent settings, page names, registration, activity`.

### Task 4: Tray agent server (Windows)

**Files:** `crates/p2c-tray/src/win/{agent_server.rs,agent_backend.rs}`, `crates/p2c-tray/src/win/{control.rs,menu.rs,icon.rs,engine.rs}`

**Interfaces:**
- Consumes: Task 1 routes, Task 2 `Server`/`Backend`, Task 3 helpers; the tray's existing `http` client, controller command channel, balloon queue, engine spawn (add env `P2C_TRAY_URL`, `P2C_TRAY_TOKEN`).
- Produces: one std `TcpListener` thread on 127.0.0.1 (agent port per Global Constraints); `/tray/agents` GET `{ allowed, registered: [{app, ok, error}], activity: [...], snippets: { stdio, url } }` (snippet url without token; header shown as `Authorization: Bearer <token from agent.json>`), POST as Task 1; `/mcp` per Global Constraints; `TrayBackend` implementing `Backend` (tray-side: Status-while-stopped, Start, Stop, ForceRestart, InstallUpdate, ListPages, ShowPage file write; others → engine admin API with 5 s timeout); menu checkbox "Allow agents"; icon badge while an agent call happened in the last 60 s; tooltip line "Agent: <name> (active)".
- [ ] **Step 1: Failing tests:** `mcp_404_when_agents_off`, `mcp_requires_bearer_and_host`, `tray_routes_require_tray_token`, `engine_tools_when_stopped_fail_fast` (< 5 s, "call start first"), `switch_off_closes_mcp_and_cleans_up` (discovery file removed, registration removal invoked via an injectable registrar), `show_page_writes_then_opens` (temp pages dir, fake engine records `/api/navigate` with the file URL), `port_falls_back_when_8766_taken` (discovery URL uses the new port), `reset_token_rotates_and_rewrites_discovery` (old token → 401).
- [ ] **Step 2–4:** run → fail; implement; tests, clippy, fmt.
- [ ] **Step 5: Commit** `feat(tray): local agent server with MCP, activity and Allow agents`.

### Task 5: `--mcp` stdio bridge

**Files:** `crates/page2cam/src/{main.rs,mcp_bridge.rs}`

**Interfaces:**
- Consumes: Task 3 `agents_allowed_in`, discovery file format; Task 2 error-message text.
- Produces: `page2cam.exe --mcp`: newline-delimited JSON-RPC on stdin/stdout (logs only to the log file, never stdout); per message POST to the discovery URL with the Bearer token; agents off → local answers (initialize ok, tools/list empty with an instructions note, tools/call → the "Allow agents" error); no tray → detached launch per Global Constraints, wait ≤ 10 s; tray gone mid-session → re-read discovery once, else tool error "Page2Cam is not running".
- [ ] **Step 1: Failing tests:** `bridge_forwards_and_preserves_ids` (in-process fake HTTP server), `bridge_agents_off_answers_locally`, `bridge_never_writes_non_jsonrpc_to_stdout`, `detached_launch_flags` (pure fn returns the three flags).
- [ ] **Step 2–4:** run → fail; implement; tests, clippy, fmt.
- [ ] **Step 5: Commit** `feat: --mcp stdio bridge for desktop agents`.

### Task 6: Admin page Agents panel, docs

**Files:** `crates/p2c-admin/ui/index.html`, `docs/agent-api.md`, `docs/openapi.json`, `packaging/windows/README.txt`, `CLAUDE.md`

- [ ] Agents panel: "Allow agents" switch, "Reset agent access", registered agents with ok/error, copy-paste snippets (stdio and URL), activity list (newest first, refresh every 5 s while visible), note "Restart Claude Desktop after switching on". Hidden with "Not available in standalone mode" when `agents_allowed` is null. Test: admin `ui_has_agents_panel` (served HTML contains the panel ids). Docs: tool list and setup per agent (incl. Codex, Claude Code CLI), OpenAPI for engine routes with token/Host rules, uninstall note (entries left in agent configs). CLAUDE.md: p2c-mcp crate, agent server, bridge, commands. Commit `feat(admin): Agents panel; agent docs`.

### Task 7: Acceptance (with the user)

- [ ] Ask the controller before running anything (user's Page2Cam must be closed). MCP Inspector (`npx @modelcontextprotocol/inspector page2cam.exe --mcp`) lists 25 tools. With Claude Desktop: switch on Allow agents (tray) → restart Claude Desktop → "start Page2Cam and show a background with today's agenda, leave room for me on the right" → start balloon, page shown, snapshot used; move overlay; start/stop a test stream (balloons); switch off → next call errors. Codex: same entry works from `config.toml`. Record results in the ledger.
