# Agent kit: templates, playbooks and saved looks: design

Status: approach B chosen by the user (2026-11-24); hands-free (the user opted out of further questions). Builds on overlay layers (`2026-11-22-overlay-layers-design.md`).

## Intent

After the one-step install (`install-windows.ps1 -AllowAgents`), any agent (Claude Desktop, Claude Code, Codex, Cursor, VS Code) should know how to run a good-looking show for whoever is using it, from one prompt: "set me up for a webinar", "I'm a physiotherapist starting a telehealth call", "we're demoing our 2.0 release to customers". Today an agent gets 37 tool schemas and a three-sentence intro; the overlay examples live only in the repo's `docs/agent-api.md`. Agents have to hand-write large HTML for every graphic, results vary, and a changing score or timer reloads its graphic.

The kit gives agents (and the person at the control page):

1. **Built-in templates**: ~40 named, tested, parameterised graphics (timers, agenda, charts, QR cards, product cards, code cards…). One short call makes one; changing a value updates it live without reloading.
2. **User templates**: the user (or an agent on their behalf) builds a template, saves it, reuses it, shares it as a file.
3. **Saved looks ("scenes")**: the current layers (plus background and webcam choices, optionally) saved under a name and recalled in one call: "intro", "main", "Q&A", "break".
4. **Playbooks**: one per user group (24, below): the right background, webcam style, templates with example values, the order of steps, do and don't, privacy/compliance rules.
5. **Delivery to every agent**: a richer MCP `instructions` text, a `guide` tool (works in every client), MCP resources and prompts (for clients that show them), and a Claude Code skill installed when agents are allowed.

Why: the user wants Page2Cam to be useful to the broad market of webcam, meeting and streaming users with an AI agent doing the setup: "install it once and the agent can do everything".

## User groups (the playbooks)

Creators: (1) game and live streamers, (2) podcasters and commentators, (3) musicians and performers, (4) cooking, craft and DIY creators, (5) fitness and yoga instructors.
Business: (6) business presenters and sales demos, (7) webinar hosts and marketers, (8) live shopping and product launches, (9) team leads and internal meetings, (10) recruiters and job interviews, (11) real estate agents, (12) financial advisers and analysts, (13) lawyers and consultants, (14) IT support and helpdesks.
Care and education: (15) doctors and telehealth, (16) therapists and counsellors, (17) teachers and tutors.
Community: (18) churches and faith groups, (19) conferences and virtual events, (20) nonprofits and fundraisers.
**Software companies** (own section, user's request): (21) product demos and launches (release announcement, "what's new", feature walk-through over a shared screen), (22) engineering rituals (sprint review/demo day, all-hands, incident status updates and post-mortems), (23) developer streams and talks (live coding, conference talks, dev-rel), (24) customer success (onboarding webinars, training, office hours, support).

Each playbook names: background (page / screen / blurred room / image / colour), webcam style, the templates with example params, a step order (intro → main → Q&A → close), timing hints, and rules. Rules that must be in the playbooks:

- **Medical, therapy, legal, finance:** never put patient/client names, diagnoses, case or account details in a graphic while recording or streaming; prefer blurred room; therapists: no flashing or busy motion. Finance: a permanent disclaimer bar while streaming. Legal: a "confidential" badge when asked.
- **Everyone:** everything is visible live in the call; check with `snapshot` after changes; hide graphics that cover the person's face; one moving element at a time; keep text inside the 5% safe margin; clear graphics at the end.

## Goals

- ~40 built-in templates (catalogue below), each rendering correctly with its example values on a transparent 1920×1080 overlay, each with a typed parameter list, defaults and an example.
- Live parameter updates without reload (score, timer target, current agenda item, poll numbers).
- User templates: create, update, delete, list, export/import as a JSON file; from an agent (MCP), from the control page, and "save this HTML layer as a template".
- Scenes: save, list, load (replace or add), delete; from MCP and the control page.
- Playbooks and the guide reachable by every agent through tools; resources/prompts for clients that support them; a Claude Code skill installed with "Allow agents".
- The one-line install + restart of the agent app is all a user needs.

## Non-goals

- A visual drag-and-drop template designer (the control page edits parameters and raw HTML; agents write HTML).
- Online template marketplace or sync (export/import a file is the sharing story for now).
- Live data connectors (stock prices, GitHub stats, Twitch follows); templates take values from the agent, which may fetch them itself. An HTML/template layer may still fetch from the internet inside its sandbox, as today.
- Speech-to-text captions (a manual caption bar template only).
- Non-Windows packaging changes.

## Anti-goals

- Templates must not weaken the overlay sandbox: template code (built-in or user) runs only in the sandboxed opaque-origin frame; parameters are data (JSON), and built-in templates set text with `textContent`, never `innerHTML` from parameters.
- No surprise on screen: loading a scene or adding a template follows today's rule (shown at once unless `visible:false`); after a restart everything comes back hidden.
- Medical/legal/finance playbooks must not encourage showing private data.
- No extra cost while nothing is visible (overlay idle pause unchanged).
- Don't bloat every agent's context: `instructions` stays short (≤ ~60 lines); the details are fetched on demand through `guide`.

## Approaches considered

- **A. Guides and examples only.** Playbooks plus raw HTML snippets the agent pastes into `html` layers. Least code; but bulky calls, uneven results between agents, and every value change reloads the graphic.
- **B. Built-in templates + guides (chosen).** Templates ship inside the exe as named, parameterised graphics with live updates; playbooks, prompts and the skill build on them. More work, but short reliable calls and consistent looks.
- C. A separate "show controller" app or web service. Dropped: adds a dependency and an online component.

## Design

### Template model (`p2c-overlay`, platform-free)

`Template { id, name, category, description, params: [Param], html, default_rect (x, y, w, h), full_frame: bool, example: {params}, builtin: bool }`.

`Param { name, type, default, help, required? }`, types: `text`, `longtext`, `number` (min/max), `bool`, `colour`, `choice` (options), `list` (of text), `table` (rows of text cells), `datetime` (ISO; for countdowns), `duration` (seconds), `url`, `asset` (a stored image), `qr` (text/URL to encode). Values are validated against the type (lengths capped, numbers clamped, colours `#rrggbb`, URLs http/https only); unknown params are refused with the field named.

**New layer kind `template`**: `{ kind: "template", template: "<id>", params: {…} }`, geometry defaults from the template. `update_layer` with `params` merges partially (`null` resets to default).

**Rendering**: like `html` layers, in the sandboxed frame `layer/<id>`. The layer page = a small trusted runtime (`P2C`) + the template's HTML. The runtime gives templates helpers: `P2C.onParams(fn)` (called with the full params at load and on each change), `P2C.text(el, s)`, time formatting, countdown ticking, number tweening, simple SVG charts (bar, line, donut, progress), a QR helper (the engine renders `qr` params to SVG path data in Rust with the `qrcode` crate (MIT/Apache) and passes it as data, so templates need no QR code of their own), and the safe-area constants. **Live updates**: the scene keeps a separate params version; when only params change it `postMessage`s the new params into the iframe (no reload, so animations and timers keep running); a template change reloads the frame.

**Built-in templates** live as files under `crates/p2c-overlay/templates/<id>.html` + `<id>.json` (meta), embedded with `include_str!` via a generated or hand-written table. Look: three themes from the existing presets (`news`, `minimal`, `brand`) as a common `theme` param, plus `accent` colour; Segoe UI / system fonts only (no web fonts).

**Catalogue** (builder may merge/rename a few; at least these capabilities):

- *Basics:* lower-third (with optional logo and second line), name-badge, live-badge (LIVE/REC/ON AIR), logo-bug (image corner), social-handles, disclaimer-bar, confidential-badge, caption-bar (manual captions).
- *Time:* clock, countdown (to a time or for a duration; ends with a message), stopwatch, interval-timer (work/rest rounds, fitness), speaker-timer (per-speaker, turns amber/red).
- *Full-frame cards:* starting-soon (with countdown), be-right-back, thank-you / ending (with socials), speaker-intro (photo, name, bio), section-title.
- *Lists and text:* agenda (current item highlighted), steps (current step), checklist, quote-card, question-card (audience Q&A), lyrics / verse card, recipe / ingredients card, now-playing.
- *Data:* bar-chart, line-chart, donut / progress, kpi-cards (2–4 numbers with change arrows), goal-bar (fundraising/follower thermometer), scoreboard (two teams, period, clock), poll-results, comparison-table, leaderboard.
- *Call to action:* qr-card (QR + text), product-card (picture, name, price, old price, offer countdown, stock bar), sponsor-carousel (rotating logos).
- *Calm:* breathing (inhale/hold/exhale animation, slow).
- *Software companies:* code-card (syntax-highlighted snippet, optional typing animation; highlighter in the runtime, small, for js/ts/py/rust/go/java/c#/sql/shell/json), terminal-card (command + output, typing), release-notes (version, date, new/improved/fixed lists), roadmap-timeline (now/next/later or dated milestones), status-card (service status: operational/degraded/outage, incident title, updates timeline), architecture-diagram (boxes and arrows from a simple node/edge list, auto layout in columns), metrics-board (MRR/DAU/uptime/latency tiles; reuse kpi-cards if equivalent), callout (arrow + label pointing at a spot on a shared screen, for demos), keyboard-shortcut (key caps), version-badge ("Now shipping v2.0").

### User templates

- Stored in `<data dir>/overlays/templates/<id>.json` (same shape as built-ins, `builtin:false`), at most 200, HTML ≤ 256 KB, ids `a-z0-9-`, may not reuse a built-in id. A user template may reference stored assets (logo) by stored name.
- Created by: MCP `save_template` (full definition), `save_template { from_layer: "<html layer id>" }` (no params), or `save_template { copy_of: "<built-in id>", … }` (start from a built-in and change it); the control page (Templates tab: new from HTML, edit params list and HTML in a text area, duplicate a built-in, delete, export to a `.p2c-template.json` file, import one).
- Imported/agent-written templates are untrusted code: same sandbox as `html` layers; validation of meta; nothing else is needed.
- Layers that use a deleted user template keep showing a "template missing" placeholder and are reported by `list_layers`.

### Scenes (saved looks)

- `Scene { id, name, layers: [layer definitions without ids' runtime state], background?: BackgroundDto, webcam?: {visible, style, x, y, width}, created }` in `<data dir>/overlays/scenes/<id>.json`, at most 100.
- `save_scene { name, include_background?, include_webcam? }` (captures the current layers, visible or not, and optionally the other two), `load_scene { id, mode: "replace" | "add" }` (replace = clear layers first; layers show as saved), `list_scenes`, `delete_scene`. Control page: a Scenes row in the Overlays panel (buttons per scene, save current, delete).
- Loading a scene that uses a missing template or asset loads the rest and says what was skipped.

### Agent delivery

- **`instructions`** (MCP initialize): ~40–60 lines: what Page2Cam is; the call rule (status → start → change → snapshot); output geometry and safe area; "everything is live in the call"; the main tool families; "for a use case, call `guide` with the user's role or goal first"; the privacy rules; where templates/scenes fit.
- **`guide` tool** `{ topic? }`: no topic → the index (playbooks by group, template catalogue summary, how-tos); `topic` = a playbook id (e.g. `telehealth`, `webinar`, `software-release`), a template id (full param list + example call), or a how-to (`templates`, `scenes`, `layout`, `privacy`, `writing-templates`). Markdown, each ≤ ~8 KB. Fuzzy matching on topic ("doctor" → telehealth).
- **Template tools**: `list_templates { category? }` (id, name, category, one-line description, built-in/user), `get_template { id }` (params with types/defaults/help, example call, and the HTML for user templates and on request for built-ins), `save_template`, `delete_template`. Adding one uses the existing `add_layer { kind: "template", template, params, … }`; changing values uses `update_layer { id, params }`.
- **Scene tools**: `save_scene`, `load_scene`, `list_scenes`, `delete_scene`.
- **MCP resources** (`resources/list`, `resources/read`; capability advertised): `page2cam://guide`, `page2cam://playbooks/<id>`, `page2cam://templates/<id>`; same text as `guide`.
- **MCP prompts** (`prompts/list`, `prompts/get`): one per playbook ("Set up for a webinar", …) with 1–3 optional arguments (name, title/company, topic), plus "Clear all graphics" and "Save this look". Each prompt text tells the agent to read the playbook via `guide` and carry it out with snapshot checks.
- **Claude Code skill**: when "Allow agents" is on, the tray writes `%USERPROFILE%\.claude\skills\page2cam\SKILL.md` (frontmatter `name: page2cam`, a description that triggers on webcam/meeting/stream/overlay/lower-third/presentation requests) plus `reference/` files (the guide index, playbooks, template catalogue) generated from the same embedded sources, with a marker so Page2Cam only ever updates/removes its own folder; refreshed when the version changes; removed when agents are switched off. Only if `%USERPROFILE%\.claude` exists (like the other registrations).
- **Repo docs**: `docs/agent-guide/` generated from the same sources (a test keeps them in sync), linked from README and `docs/agent-api.md`; README gets a short "What agents can do for you" section with a few example prompts per group.

### Single source

Playbooks, how-tos and template metadata are files in the repo (`crates/p2c-overlay/templates/`, `crates/p2c-mcp/guide/` or a new small crate `p2c-guide`, builder's call), embedded at build time, and used by `instructions`, `guide`, resources, prompts, the skill and the repo docs. Nothing is written twice by hand.

### Control page

Overlays panel grows three tabs: **Layers** (today's list), **Templates** (gallery by category with search; click → parameter form generated from the param types → Add; user templates editable/deletable; New / Duplicate / Import / Export; "save HTML layer as template" in a layer's menu), **Scenes** (buttons to load, Save current, delete). Selecting a template layer shows its parameter form for live edits.

### Admin API

`GET /api/templates[?category=]`, `GET /api/templates/<id>`, `POST /api/templates` (save), `DELETE /api/templates/<id>`, `GET/POST /api/scenes`, `POST /api/scenes/<id>/load`, `DELETE /api/scenes/<id>`; token + Host checked like the rest; OpenAPI updated.

## Constraints

- Windows 11 is the release target; all of the above is platform-free except the skill path (works anywhere `~/.claude` exists).
- GPL-3.0-or-later; new crates/dependencies must be compatible (e.g. `qrcode` MIT/Apache); third-party notices updated.
- No new runtime downloads; everything is embedded in the exe.
- Output budget unchanged: a visible template layer must keep the output at 30 fps on the user's laptop (same as an `html` layer).
- Tool count grows from 37 to ~47: tool descriptions stay short; detail lives in `guide`.

## Testing

- Unit: template meta valid (param types, defaults, examples validate), param validation and merge, user template store (limits, ids, built-in collision, import/export round trip), scene save/load (replace/add, missing template), guide topics resolve (every playbook, template, how-to; fuzzy aliases), MCP resources/prompts/tools schemas, skill files written/updated/removed only in its own marked folder (temp dirs only).
- Render (real CEF, `p2c-browser --test render`): every built-in template with its example params renders on the transparent overlay without console errors, draws inside its rect, and a params update via postMessage changes pixels without a reload.
- Docs sync test: `docs/agent-guide/` equals what the sources generate.

## Left to the builder

- Exact template designs, animations, and the final catalogue names (capabilities above are required).
- Crate layout for the guide content; how built-in templates are embedded.
- The scene/params message format; parameter form widgets in the control page.
- Fuzzy topic matching method.
- Whether metrics-board and kpi-cards are one template.
