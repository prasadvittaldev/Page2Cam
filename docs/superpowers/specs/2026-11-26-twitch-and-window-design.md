# Twitch: stream to YouTube and Twitch at once, and a window as the background

Date: 2026-11-26. Status: approved by the user (2026-11-26).

## Intent

The user wants Page2Cam to work for **reaction and watch-along streaming**. Someone else's Twitch stream, game, or other content plays full screen behind them. They react in front of it, cut out over it, and the result goes live on **YouTube, Twitch, both at once, or neither**. They also want it for **their own games**.

Two features serve this:

1. **Several stream destinations.** YouTube and Twitch, each with a tick box. Any combination can be ticked, including none.
2. **Window background.** A new full-frame background shows one chosen application window live (Twitch in the user's browser, a game, any app), with **that app's sound** sent to the stream.

The user decides through the control page. Agents decide through MCP, with the same choices.

## Findings that shaped this

- **Streaming today** goes to one RTMP target: `rtmp_url` (default YouTube) plus one key in the OS keyring. One ffmpeg process does it: raw video on stdin, audio over a local TCP socket, H.264 and AAC in FLV (`p2c-stream`).
- **Page2Cam's Chromium cannot play Twitch.** This was measured on 2026-11-26 with the shipped CEF build: `MediaSource.isTypeSupported` returns false for H.264 and AAC, and true for VP9 and AV1. Twitch delivers only H.264 and AAC, so its player inside the background page would stay black. The embed itself loads fine from `http://127.0.0.1` (tested in Chrome). The codec is the blocker, not the embed rules.
- **The screen background already does almost all of window capture.** `p2c-screen` uses Windows Graphics Capture on a monitor and has a frame pump into the page slot and the GPU `upload_frame`. `screen.rs` pauses Chromium's paints while capture runs. A window is a different capture target on the same path.
- **Page sound goes to the stream only, never to the speakers** (an existing rule). The user was offered a "hear page sound" switch and **declined it**. They listen to the content themselves in its own app, which window capture makes natural.

## Goals

- Stream to YouTube only, Twitch only, both, or neither. The video is encoded **once**, whatever is ticked.
- If one site fails or drops, the other carries on. Only the failed site reconnects. Status shows each site separately ("YouTube live · Twitch reconnecting").
- Sites can be ticked or unticked while live. That starts or stops only that site's sender.
- Window background: choose a window from a list. It shows live behind the person, letterboxed like a page, with only that app's sound going to the stream as page sound (same on/off and volume as page sound).
- The chosen window is found again after a restart, or after the app is reopened.
- Agents can do everything the control page can, except read or set stream keys.

## Non-goals

- Hearing page or window sound through Page2Cam. The user declined this.
- Sites other than YouTube and Twitch (Kick, Facebook, a third custom server). The design should not make a third one hard, but none is built now.
- Playing Twitch links inside Page2Cam's browser (codec, see above), or fetching Twitch streams with ffmpeg.
- Twitch chat on screen.
- Window background on Linux. It fails with "window capture is Windows-only for now", like the screen background. **Multi-site streaming works on Linux too.**

## Anti-goals (would be a failure even if it "works")

- One site's failure stops or restarts the other.
- Streaming to two sites doubles the encoding work on GPU or CPU.
- The window background captures other apps' sound (Discord, notifications, the user's music).
- A stream key appears in logs, the status, agent answers or error messages.

## Approaches considered

**Sending to several sites** (the user picked 1):
1. **Encode once, one sender per site** *(picked)*. One encoder ffmpeg writes the encoded stream to Page2Cam. A fan-out thread copies it to one lightweight "copy" ffmpeg per site (`-c copy`, no re-encode), each pushing to its RTMP server. Each site has its own state and reconnects on its own.
2. ffmpeg's `tee` output in one process. Less code, but a dropped site stays dropped until the whole stream restarts, and the status can't tell which site failed.
3. Two complete encoders. Simple, but doubles the encoding load.

**Twitch as a background** (the user picked 1):
1. **Window capture plus that app's sound** *(picked)*. Works for Twitch, any site and games, needs no codecs, and fits "I'll listen myself".
2. Page2Cam plays the Twitch link itself through ffmpeg. That relies on an unofficial Twitch interface that breaks periodically, or on bundling a downloader.
3. A Chromium build with H.264. Hours of building per release, plus patent licensing. Dropped.

## Design

### 1. Stream destinations

**Settings** (`config.toml`):

| key | default | meaning |
|---|---|---|
| `rtmp_url` | `rtmp://a.rtmp.youtube.com/live2` | YouTube's server. Existing key, unchanged meaning, so existing configs keep working. |
| `twitch_url` | `rtmp://live.twitch.tv/app` | Twitch's server (Twitch's auto-select ingest). |
| `stream_youtube` | `true` | YouTube ticked |
| `stream_twitch` | `false` | Twitch ticked |

- **Keys:** the existing keyring entry stays YouTube's key. Twitch gets its own keyring entry. Both use the same "remember" behaviour as today.
- **Servers:** each site's server is editable under an "Advanced" fold in the control page, so a custom RTMP server still works by editing YouTube's or Twitch's server.

**Pipeline:**
- **Encoder.** The existing ffmpeg command, unchanged up to its output. It writes **MPEG-TS to stdout** instead of FLV to RTMP. Encoder choice, the probe and the x264 fallback all stay.
  - Keyframe interval is 2 s (check `encoder_options`; Twitch requires ≤ 2 s).
  - CBR at `video_kbps`.
- **Fan-out thread.** It reads the encoder's stdout and hands each chunk to every active sender through a **bounded** per-sender queue. When a sender's queue is full, that sender is restarted (it can't keep up). The encoder and the other senders are never blocked.
- **Sender per site:** `ffmpeg -f mpegts -i pipe:0 -c copy -f flv <server>/<key>`.
  - A newly started sender (start, reconnect, or a site ticked while live) receives data from the next keyframe onward. The builder decides how: watch TS random-access flags, or rely on ffmpeg and verify on YouTube and Twitch.
- **Per-site state:** `idle | connecting | live | reconnecting (attempt n) | error: <reason>`. Live/connecting are parsed from each sender's stderr, as today.
  - A sender that exits while the stream is running is restarted after 2, 4, 8, 16, then 30 s (cap), while that site stays ticked.
  - An auth error (bad key) goes to `error` with no retry storm. The builder picks the stderr patterns.
- **Overall `stream` state:**
  - `live` if any site is live;
  - `connecting` while none is live yet;
  - `error` if the encoder fails;
  - `idle` when stopped.
  - The encoder failing is a whole-stream error, handled as today (x264 fallback first).
  - *(builder-check call)* When **every** ticked site is in `error` (not reconnecting, for example bad keys on both), the stream stops by itself. The overall state is `error: YouTube: <reason>; Twitch: <reason>`, so the stream balloon and the tray health show it, and `status` tells an agent. A site that is only reconnecting keeps the stream running.
- **Start rules:**
  - Start with no site ticked fails with "Choose YouTube and/or Twitch."
  - Start with a ticked site that has no key fails with "Twitch has no stream key", and nothing starts.
  - Unticking the last site while live stops the stream.
- **Bitrate:** if Twitch is ticked and `video_kbps` > 6000, the control page shows a note: "Twitch accepts up to 6000 kbps; lower 'video bitrate' or Twitch may refuse the stream." There is no automatic change, because one encode serves both sites.
- **Redaction:** `redact()` covers both keys in every log line, error and status.

**Status:** `Status.stream` keeps its meaning (overall). A new `Status.streams: [{site: "youtube"|"twitch", state, detail}]` lists the ticked sites. The admin `StreamRequest` becomes `{sites: {youtube?: {key?, remember?, url?}, twitch?: {…}}}`, or an equivalent the builder prefers. The old `{url, key, remember}` form is still accepted as YouTube.

**Control page (stream panel):**

```
Stream                                         [ Start ]
 [x] YouTube   key ●●●●●● (saved)   YouTube live
 [x] Twitch    key [_____________]  Twitch reconnecting (2)…
 ▸ Advanced: servers, video bitrate
```

**MCP:**
- `start_stream {sites?: ["youtube","twitch"]}`: without `sites`, the saved ticks. With `sites`, those ticks are saved and used.
- `stop_stream {sites?}`: without `sites`, stops everything. With `sites`, stops only those sites (leaving the stream running if any remain).
- `set_stream_sites {youtube?: bool, twitch?: bool}`: changes the saved ticks, and while live, starts or stops senders.
- `status` includes `streams`.
- Agents still can't pass or read keys. A ticked site with no key gives the user-facing message, so the agent tells the user to enter it in the control page.

### 2. Window background

**Choice:** a new full-frame background, `window`, next to `screen`. It forces `cutout-full` like the other full frames.

**Settings:** `background_window` is a **window match**: `{exe = "chrome.exe", title = "…"}`, stored as a TOML inline table or two keys (builder's choice). An HWND isn't stable across restarts, so the match is by exe name plus title.

**Matching** *(builder-check call, privacy: a near miss must never put another window, such as the user's email, on camera)*:
- **While running,** capture follows the window handle it started with, so title changes (a browser tab's title) don't matter. The saved title is updated as it changes (written at most every 10 s).
- **After a restart, or when the window is gone,** only a window with the **same exe and exactly the saved title** resumes by itself. Anything else gives "window not found: <title> (<exe>): choose it again". The engine keeps checking for an exact match every 2 s.
- **There is no similarity matching.** An agent's `window: <title text>` must match exactly one listed window (exact title, else a case-insensitive substring that is unique). Otherwise it gets an error listing the candidates.

**Listing:** `GET /api/windows` and MCP `list_windows` list `{id (hwnd), title, exe, app name, size, minimized}`. The list contains visible, top-level, non-tool, non-cloaked windows with a title. It leaves out Page2Cam's own windows and the shell (taskbar, desktop).

**Capture** (in `p2c-screen`, extended):
- Windows Graphics Capture `CreateForWindow`, on the same frame pool, thread, staging and BGRA path as the monitor capture, at most `fps`.
- The frame size follows the window. The frame pool is recreated on a size change, and the compositor letterboxes any size, as it does for screens.
- **Cursor off** for windows (it is on for screens).
- **Border:** request borderless capture where Windows allows it (Win11 `GraphicsCaptureAccess`, `IsBorderRequired = false`). Otherwise Windows draws its yellow border, as for screens today.
- **Minimized window:** no frames arrive. Keep the last frame and show the note "window minimized: restore it (it can sit behind other windows)".
- **Closed window:** the note "window closed", then a black frame, then the matching loop above.
- Exclusive-fullscreen games can't be captured. Borderless/windowed can. That goes in the how-to and the guide.

**Sound:**
- WASAPI **process loopback** (`AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS`, `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`) on the process that owns the window. That covers browsers, whose sound comes from a child process.
- It is resampled to 48 kHz stereo and goes into an `AudioRing` mixed as **page sound** (same `page_on`/`page_gain`).
- If loopback fails, the picture still works and the note says "window sound unavailable: <reason>".
- **Only that process tree is captured.** Nothing else on the PC.
- **Browsers: the whole browser's sound** *(user decision, 2026-11-26)*. Windows can't capture one tab. For a browser window, the capture includes every tab and browser-based app in that browser (music, a Meet or Discord call). The user accepted this with a clear warning:
  - the window choice and the note line say "Sound: everything playing in <app>";
  - the guide, playbook and how-to advise a **second browser just for the content** (e.g. Edge for Twitch, Chrome for everything else), and never having a call open in that browser;
  - agents get the same warning in the `set_background` answer when the window's exe is a known browser (chrome, msedge, firefox, brave, opera, vivaldi).

**Engine:**
- Like `screen.rs`: Chromium's paints are dropped while the window background runs; switching away stops capture and sound first, then resumes Chromium with a repaint.
- `background_note` carries the notes above. In GPU mode, frames go through `PageHolder::upload_frame` like the screen's.

**Privacy and agents:**
- `set_background {full: "window", window: <id | title text>}`. A title text uses the same matching as above.
- `list_windows` reveals window titles to the agent. The tool description says so, and the MCP redaction is unchanged.
- The tray balloon "<agent> is sharing a window: <title>" always shows, like the screen one.

**Control page (background panel):** a "Window" choice with a dropdown of windows (title and app) plus a refresh button, and the note line.

### 3. Agent guide and docs

- A playbook for **reaction and watch-along streamers**:
  - Window background with Twitch or YouTube in the browser (theater or full screen), full cut-out, both sites ticked.
  - Rules: the user listens in their own browser; mute the mic when not talking if the speakers are loud; don't rebroadcast content they have no right to.
- The gaming playbook (if one exists, else in the same one): the game window plus game sound; borderless mode, not exclusive fullscreen.
- Updates:
  - `instructions.md`, the `set_background` and stream tool descriptions;
  - `docs/agent-api.md`, `docs/agent-guide/` (regenerated);
  - the README feature list;
  - CLAUDE.md (architecture: fan-out and senders, `window` background, process loopback).
- AGENT-INSTALL.md is unchanged.

## Testing (what proves it)

- **Unit:**
  - encoder args (TS out);
  - sender args;
  - per-site state machine and backoff;
  - start rules (none ticked, missing key);
  - redaction of both keys;
  - window matching (exact, best title, none);
  - config defaults and migration (`rtmp_url` still YouTube).
- **Local RTMP integration** (extends `rtmp_local`, ignored test):
  - two local ffmpeg RTMP listeners, both receive and ffprobe the same stream;
  - kill one listener: the other keeps receiving, the killed site goes `reconnecting`, a restarted listener gets the stream again;
  - tick or untick a site while live.
- **Windows (ignored tests):**
  - capture a test window the test creates (known colour, resized mid-test);
  - process loopback of a child process playing a tone (sound arrives, the PC's other sound doesn't);
  - minimized and closed notes.
- **Real check on the user's laptop:**
  - YouTube plus Twitch at once with real keys;
  - Twitch in Chrome as the window background with its sound on the stream;
  - a game window.

## Left to the builder

- The admin API shape for multi-site requests (keeping the old form working).
- How a new sender starts at a keyframe.
- The stderr patterns for auth and other errors.
- Queue sizes.
- How `background_window` is stored.
- Exact window-list filtering.
- The best-title similarity measure.
- Whether borderless capture can be requested for an unpackaged app (fall back to the border).
- How the sound tool descriptions read.
