# Twitch and Window Background Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stream to YouTube, Twitch, both or neither from one encode, and show any application window (with that app's sound) as the full-frame background.

**Architecture:**
- **Streaming.** `p2c-stream` splits today's single ffmpeg into:
  - one **encoder** ffmpeg (raw video and audio in, MPEG-TS on stdout);
  - a **fan-out** thread;
  - one **sender** ffmpeg per site (`-c copy` to FLV/RTMP), each with its own state and backoff.
- **Window background.** `p2c-screen` gains window listing, matching and Windows Graphics Capture of a window. `p2c-audio` gains WASAPI process loopback. The engine adds `FullBg::Window` beside `FullBg::Screen`, and `window.rs` is modelled on `screen.rs`.

**Tech Stack:** Rust (workspace), ffmpeg child processes, `windows` crate (WinRT Graphics Capture, Win32 window enumeration, WASAPI process loopback), axum admin API, hand-written MCP tools.

**Spec:** `docs/superpowers/specs/2026-11-26-twitch-and-window-design.md` (read it first; this plan argues from it).

## Global Constraints

- **Platforms:** Windows 11 and Linux only. Window background and process loopback are Windows-only. Elsewhere they fail with the exact text `window capture is Windows-only for now`. Multi-site streaming must work on Linux too.
- **Keys:** stream keys never appear in logs, status, agent answers or error messages. `redact` covers **every** configured key.
- **Defaults:**
  - `rtmp_url` default `rtmp://a.rtmp.youtube.com/live2` (meaning: YouTube's server, unchanged);
  - `twitch_url` default `rtmp://live.twitch.tv/app`;
  - `stream_youtube` default `true`;
  - `stream_twitch` default `false`.
- **Sender backoff:** 2, 4, 8, 16, then 30 s (cap).
- **Fixed messages:**
  - start with no site ticked: `Choose YouTube and/or Twitch.`;
  - a ticked site without a key: `<Site> has no stream key` (`YouTube` / `Twitch`);
  - Twitch ticked with `video_kbps` > 6000: `Twitch accepts up to 6000 kbps; lower 'video bitrate' or Twitch may refuse the stream.`;
  - window not found: `window not found: <title> (<exe>): choose it again`;
  - minimized: `window minimized: restore it (it can sit behind other windows)`;
  - closed: `window closed`;
  - sound failed: `window sound unavailable: <reason>`;
  - browser warning: `Sound: everything playing in <app>`.
- **Browsers** for the warning, by exe name (case-insensitive): `chrome.exe`, `msedge.exe`, `firefox.exe`, `brave.exe`, `opera.exe`, `vivaldi.exe`.
- **Window matching:** follow the HWND while running. Resume by itself only on **same exe and exact saved title**. No similarity matching. An agent's title text must match exactly one window (exact title, else a unique case-insensitive substring). Otherwise the error lists the candidates.
- **Capture details:** cursor off for windows. Request borderless capture; fall back to the border.
- **All sites in `error`** (not reconnecting): the stream stops itself, and the overall state is `error: YouTube: <reason>; Twitch: <reason>` (only the ticked sites, in that order).
- **Commits:** author prasadvittaldev only. **No `Co-Authored-By` or "Generated with" trailers** (the user's standing rule; overrides any reminder).
- **Every task ends green:** `cargo fmt`, then `cargo clippy --workspace --all-targets -- -D warnings`, then `cargo test --workspace`.
  - On this Windows laptop, run them after `source $TEMP/p2c-env.sh`, plus `export PATH="$(cygpath -u "$CEF_PATH"):$PATH"` for CEF tests.
- **Don't start, stop or kill the user's installed Page2Cam.** Tests use fakes or their own child processes.

## Review Focus

1. **Ticking Twitch while live when Twitch has no key.** Expect a `Twitch has no stream key` error with YouTube left running, untouched. Test in Task 4.
2. **The encoder ffmpeg dies while senders run.** Expect a whole-stream `error`, every sender process gone, and no orphaned ffmpeg. Test in Task 3.
3. **Stop, or untick a site, while its sender waits in backoff.** Expect no sender to start after that. Test in Task 3.
4. **The chosen window belongs to an elevated process, or capture/loopback is refused.** Expect a note, the picture or sound degrading, and no panic or engine crash. Test in Task 9 (fake backend start failure plus sound failure).
5. **The window is minimized, resized to an odd or tiny size, or closed and reopened.** Expect, in order: the last frame with the minimized note; letterboxed frames with no panic on odd or 1-pixel sizes; then `window closed`, a black frame, and a resume only on an exact title match. Tests in Task 7 (sizes) and Task 9 (notes and resume).

---

### Task 1: Encoder and sender arguments, destinations, multi-key redaction

**Files:**
- Modify: `crates/p2c-stream/src/args.rs`, `crates/p2c-stream/src/lib.rs`
- Test: `crates/p2c-stream/src/args.rs` (unit tests module)

**Interfaces:**
- Produces:
  - `pub enum Site { YouTube, Twitch }`, with `fn as_str(self) -> &'static str` (`"youtube"`/`"twitch"`), `fn label(self) -> &'static str` (`"YouTube"`/`"Twitch"`), and serde lowercase.
  - `#[derive(Clone, Debug)] pub struct Destination { pub site: Site, pub url: String, pub key: String }`, with `fn target(&self) -> String` (today's `target_url` logic).
  - `StreamConfig`: `url`/`key` are replaced by `pub destinations: Vec<Destination>`. The other fields are unchanged.
  - `pub fn encoder_args(cfg: &StreamConfig, audio_port: u16) -> Vec<String>`: today's `ffmpeg_args` with the output changed to `-f mpegts -mpegts_flags +resend_headers pipe:1`. `ffmpeg_args` is removed.
  - `pub fn sender_args(target: &str) -> Vec<String>`: `-hide_banner -loglevel info -stats_period 1 -f mpegts -i pipe:0 -c copy -f flv <target>`.
  - `pub fn redact_all(s: &str, keys: &[&str]) -> String`: `redact` for each non-empty key.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn encoder_writes_mpegts_to_stdout() {
    let a = encoder_args(&cfg_with(vec![]), 5000);
    let n = a.len();
    assert_eq!(&a[n - 5..], ["-f", "mpegts", "-mpegts_flags", "+resend_headers", "pipe:1"]);
    assert!(!a.iter().any(|s| s.starts_with("rtmp")));
    assert!(a.windows(2).any(|w| w == ["-g", "60"]), "2 s keyframes at 30 fps");
}
#[test]
fn sender_copies_to_flv() {
    let t = "rtmp://live.twitch.tv/app/live_123";
    let a = sender_args(t);
    assert!(a.ends_with(&["-c", "copy", "-f", "flv", t].map(String::from)));
    assert!(a.windows(2).any(|w| w == ["-i", "pipe:0"]));
}
#[test]
fn every_key_is_redacted() {
    let s = "push rtmp://a/live2/yt-key-1 and rtmp://b/app/live_tw_2";
    let r = redact_all(s, &["yt-key-1", "live_tw_2", ""]);
    assert!(!r.contains("yt-key-1") && !r.contains("live_tw_2"));
}
#[test]
fn destination_target_appends_the_key() {
    let d = Destination { site: Site::Twitch, url: "rtmp://live.twitch.tv/app/".into(), key: "k".into() };
    assert_eq!(d.target(), "rtmp://live.twitch.tv/app/k");
}
```

(The `-g 60` check assumes `cfg_with` uses fps 30. Move the existing `ffmpeg_args` tests over to `encoder_args`.)

- [ ] **Step 2: Run the tests to make sure they fail.** Run `cargo test -p p2c-stream --lib args`. Expected: compile errors for `encoder_args`, `sender_args`, `redact_all`, `Destination`.
- [ ] **Step 3: Implement.** Write the signatures above, update `lib.rs` re-exports, and make the controller compile against `destinations` (temporarily use `destinations[0]`; Task 3 replaces this).
- [ ] **Step 4: Run the tests to make sure they pass.** Run `cargo test -p p2c-stream --lib`. Expected: all pass.
- [ ] **Step 5: Commit.** `git commit -am "stream: encoder writes MPEG-TS, per-site sender args, destinations, every key redacted"`

---

### Task 2: Per-site state, backoff, exit classification, overall state

**Files:**
- Create: `crates/p2c-stream/src/sites.rs` (pure logic, no processes)
- Modify: `crates/p2c-stream/src/lib.rs`

**Interfaces:**
- Consumes: `Site` (Task 1), `StreamState` (existing).
- Produces:
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub enum SiteState { Idle, Connecting, Live, Reconnecting { attempt: u32 }, Error(String) }` with `Display`: `idle`, `connecting`, `live`, `reconnecting (n)`, `error: …`.
  - `pub fn retry_delay(attempt: u32) -> Duration`: attempts 1, 2, 3, 4 give 2, 4, 8, 16 s; 5 and later give 30 s.
  - `pub enum SenderExit { Auth(String), Other(String) }` and `pub fn classify_exit(lines: &[String]) -> SenderExit`. Auth covers, case-insensitively: `401`, `403`, `unauthorized`, `forbidden`, `invalid stream key`, `authentication`, `NetStream.Publish.BadName`. The reason is the last meaningful line.
  - `pub fn overall(sites: &[(Site, SiteState)], encoder_error: Option<&str>) -> (StreamState, bool /*stop*/)`:
    1. encoder error → `Error(e)`, stop;
    2. any `Live` → `Live`;
    3. all `Error` → `Error("YouTube: …; Twitch: …")`, stop;
    4. otherwise → `Connecting`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn backoff_doubles_to_thirty() {
    let s: Vec<u64> = (1..=7).map(|a| retry_delay(a).as_secs()).collect();
    assert_eq!(s, [2, 4, 8, 16, 30, 30, 30]);
}
#[test] fn bad_key_is_auth() {
    let l = vec!["[rtmp @ 0x1] Server error: NetStream.Publish.BadName".to_string()];
    assert!(matches!(classify_exit(&l), SenderExit::Auth(_)));
    let n = vec!["Connection to tcp://x:1935 failed: Connection refused".to_string()];
    assert!(matches!(classify_exit(&n), SenderExit::Other(_)));
}
#[test] fn overall_state_rules() {
    use SiteState::*;
    let yt = |s| (Site::YouTube, s); let tw = |s| (Site::Twitch, s);
    assert_eq!(overall(&[yt(Live), tw(Reconnecting{attempt:2})], None), (StreamState::Live, false));
    assert_eq!(overall(&[yt(Connecting), tw(Error("x".into()))], None), (StreamState::Connecting, false));
    assert_eq!(overall(&[yt(Error("bad key".into())), tw(Error("refused".into()))], None),
        (StreamState::Error("YouTube: bad key; Twitch: refused".into()), true));
    assert_eq!(overall(&[yt(Live)], Some("encoder died")), (StreamState::Error("encoder died".into()), true));
}
```

- [ ] **Step 2: Run the tests to make sure they fail.** Run `cargo test -p p2c-stream --lib sites`. Expected: unresolved items.
- [ ] **Step 3: Implement** in `sites.rs` and re-export from `lib.rs`.
- [ ] **Step 4: Run the tests to make sure they pass.** Run `cargo test -p p2c-stream --lib sites`.
- [ ] **Step 5: Commit.** `git commit -am "stream: per-site state, backoff, exit classification, overall state"`

---

### Task 3: Fan-out, senders and the multi-site controller

**Files:**
- Create: `crates/p2c-stream/src/ts.rs` (MPEG-TS keyframe detection), `crates/p2c-stream/src/fanout.rs` (fan-out thread and sender supervisor)
- Modify: `crates/p2c-stream/src/controller.rs`, `crates/p2c-stream/src/lib.rs`
- Test: unit tests in `ts.rs`/`fanout.rs`; `crates/p2c-stream/tests/rtmp_local.rs` (ignored integration)

**Interfaces:**
- Consumes: Tasks 1 and 2.
- Produces:
  - `pub fn is_random_access(pkt: &[u8]) -> bool` (`ts.rs`). It takes a 188-byte packet. It returns true when the sync byte is 0x47, there is an adaptation field (`pkt[3] & 0x20`) of length > 0, and `random_access_indicator` (`pkt[5] & 0x40`) is set.
  - `const TS: usize = 188`. The fan-out reads stdout in multiples of 188 bytes (`TS * 348` ≈ 64 KiB) and keeps 188-byte alignment across reads.
  - `const SENDER_QUEUE: usize = 64` chunks per sender. When a queue is full, that sender is restarted, counting as an `Other` exit.
  - A new sender receives nothing until a packet with `is_random_access` arrives. Before that, it gets the cached PAT/PMT packets (PID 0 and the PMT PID taken from the PAT), then data from that packet on.
  - `StreamController::start(&self, cfg: StreamConfig, ffmpeg: &Path) -> anyhow::Result<()>`, same signature. With `cfg.destinations` empty it fails with `Choose YouTube and/or Twitch.`
  - `StreamController::set_destinations(&self, d: Vec<Destination>) -> anyhow::Result<()>`. While running, it starts senders for new sites and stops the senders of removed sites. An empty list stops the stream. While idle it does nothing.
  - `StreamController::sites(&self) -> Vec<(Site, SiteState)>`.
  - `StreamController::state()` gives the overall state (Task 2 `overall`).
  - x264 retry: still triggered by the **encoder's** stderr, as today.
  - A sender's exit: `Auth` → `Error`, no retry. `Other` → `Reconnecting{attempt}` and a restart after `retry_delay`. After the sender is live, the attempt counter resets.
  - When `overall` says stop, call `stop()`, but keep the error state, so `state()` shows it.

- [ ] **Step 1: Write the failing unit tests**

```rust
// ts.rs
#[test] fn random_access_flag_is_read() {
    let mut p = [0u8; 188]; p[0] = 0x47; p[3] = 0x30; p[4] = 7; p[5] = 0x40;
    assert!(is_random_access(&p));
    p[5] = 0; assert!(!is_random_access(&p));
    p[3] = 0x10; p[5] = 0x40; assert!(!is_random_access(&p), "no adaptation field");
}
// controller.rs tests (fake ffmpeg = the test binary's own helper, as existing tests do)
#[test] fn no_destination_is_refused() {
    let c = StreamController::new(test_mixer());
    let e = c.start(cfg_with(vec![]), &ffmpeg_or_skip()).unwrap_err().to_string();
    assert_eq!(e, "Choose YouTube and/or Twitch.");
}
```

- [ ] **Step 2: Write the failing integration tests** in `tests/rtmp_local.rs`, marked `#[ignore]`. They need ffmpeg and use two local `ffmpeg -listen 1 -f flv -i rtmp://127.0.0.1:<p>/live/k` listeners.
  - `both_sites_receive_one_encode`: two listeners each record 6 s. ffprobe both files: H.264 video and AAC audio in each. Only one encoder process exists (counted by the controller's process count, `#[cfg(test)]` accessor `encoder_count() == 1`).
  - `one_site_dropping_leaves_the_other_live`: kill listener B after 3 s. `sites()` shows B `Reconnecting{..}`, A stays `Live`, and A's file keeps growing. Restart B: within `retry_delay` + 5 s, B is `Live` again.
  - `untick_and_tick_while_live`: `set_destinations(vec![a])` gives B `Idle` with no B sender process. Then `set_destinations(vec![a, b])` gives B `Live` again.
  - `encoder_death_stops_everything` (Review Focus 2): kill the encoder child (`#[cfg(test)] kill_encoder()`). `state()` becomes `Error(..)` within 3 s, and `sender_count() == 0`.
  - `stop_during_backoff_starts_nothing` (Review Focus 3): B points at a closed port, giving `Reconnecting{1}`. Call `stop()`, then wait 3 s: `sender_count() == 0` and `sites()` is all `Idle`.
- [ ] **Step 3: Run the tests to make sure they fail.** Run `cargo test -p p2c-stream` (unit), then `cargo test -p p2c-stream --test rtmp_local -- --ignored`. Expected: the new tests fail or don't compile.
- [ ] **Step 4: Implement** `ts.rs`, `fanout.rs` and the controller changes. Keep `write_video`/`write_audio` as they are. The encoder's stdout becomes `Stdio::piped()`. Use `redact_all` with every destination key on all logged lines.
- [ ] **Step 5: Run all the tests to make sure they pass.** Run `cargo test -p p2c-stream`, then `cargo test -p p2c-stream --test rtmp_local -- --ignored`. Expected: all pass, including the existing local RTMP test (moved to one destination).
- [ ] **Step 6: Commit.** `git commit -am "stream: one encode fanned out to a sender per site; each site reconnects on its own"`

---

### Task 4: Engine settings, keys, start rules, status and admin API

**Files:**
- Modify:
  - `crates/page2cam/src/config.rs` (keys `twitch_url`, `stream_youtube`, `stream_twitch`);
  - `crates/page2cam/src/app.rs` (keys per site, `start_stream`, `set_stream_sites`, status);
  - `crates/p2c-admin/src/api.rs`;
  - `crates/p2c-admin/src/server.rs` (route `POST /api/stream/sites`; `/api/stream/start` accepts the new and old bodies);
  - `crates/page2cam/src/main.rs` (wiring).
- Test: `app.rs` tests, `server.rs` tests, `config.rs` tests.

**Interfaces:**
- Consumes: `Site`, `Destination`, `StreamController::{start, set_destinations, sites}`.
- Produces:
  - **Keyring entries:**
    - YouTube keeps the existing `(KEYRING_SERVICE, KEYRING_USER)`;
    - Twitch uses `(KEYRING_SERVICE, "twitch-stream-key")`;
    - `fn saved_key(site: Site) -> Option<String>` and `fn save_key(site: Site, key: &str)`.
  - **`api.rs` types:**
    - `pub struct SiteRequest { #[serde(default)] pub on: Option<bool>, #[serde(default)] pub key: String, #[serde(default)] pub remember: bool, #[serde(default)] pub url: Option<String> }`;
    - `StreamRequest`: `#[serde(untagged)]`-compatible, accepting `{sites: {youtube?: SiteRequest, twitch?: SiteRequest}}` or the legacy `{url, key, remember}` (= YouTube on). Expose `fn normalized(self) -> SitesRequest` where `pub struct SitesRequest { pub youtube: Option<SiteRequest>, pub twitch: Option<SiteRequest> }`.
    - `pub struct SiteStatusDto { pub site: String, pub state: String, pub ticked: bool, pub has_saved_key: bool, pub url: String }`.
    - `Status` gains `pub streams: Vec<SiteStatusDto>` (always both sites, in the order youtube, twitch) and `pub stream_note: Option<String>` (the Twitch bitrate note). `stream_url` and `has_saved_key` stay, describing YouTube, for old clients.
  - **`Controls` trait:** `fn set_stream_sites(&self, req: SitesRequest) -> anyhow::Result<()>`. Tick changes are saved; while live, they apply through `set_destinations`.
  - **Start rules:** saved ticks are merged with the request.
    - None ticked → `Choose YouTube and/or Twitch.`
    - A ticked site without a request key or saved key → `<Label> has no stream key` (nothing starts).
    - A new key is saved when `remember`.
    - Ticks and server URLs are saved with `save_values` (keys `stream_youtube`, `stream_twitch`, `rtmp_url`, `twitch_url`).

- [ ] **Step 1: Write the failing tests**

```rust
// config.rs
#[test] fn stream_site_defaults() {
    let c = Config::default();
    assert_eq!((c.rtmp_url.as_str(), c.twitch_url.as_str(), c.stream_youtube, c.stream_twitch),
        ("rtmp://a.rtmp.youtube.com/live2", "rtmp://live.twitch.tv/app", true, false));
}
// app.rs (keys through a fake key store injected in tests; app.rs adds `trait KeyStore` with the keyring impl)
#[test] fn start_rules() {
    let app = test_app();
    assert_eq!(app.start_stream(sites(false, false)).unwrap_err().to_string(), "Choose YouTube and/or Twitch.");
    assert_eq!(app.start_stream(sites_tw_only_no_key()).unwrap_err().to_string(), "Twitch has no stream key");
}
#[test] fn ticking_twitch_without_key_while_live_leaves_youtube() { // Review Focus 1
    let app = live_app_with_youtube();
    let e = app.set_stream_sites(tick_twitch()).unwrap_err().to_string();
    assert_eq!(e, "Twitch has no stream key");
    assert_eq!(app.status().streams[0].state, "live");
    assert!(!app.status().streams[1].ticked);
}
#[test] fn twitch_bitrate_note() {
    let app = test_app_with_kbps(8000);
    app.set_stream_sites(tick_twitch_with_key()).unwrap();
    assert_eq!(app.status().stream_note.as_deref(),
        Some("Twitch accepts up to 6000 kbps; lower 'video bitrate' or Twitch may refuse the stream."));
}
// server.rs
// stream_start_accepts_legacy_and_sites_bodies: both bodies reach Fake as SitesRequest (legacy -> youtube on, key "k-1").
// stream_sites_route_saves_ticks: POST /api/stream/sites {"twitch":{"on":true}} -> Fake.set_stream_sites called; 400 {error} on Err.
```

(`live_app_with_youtube` uses a fake `StreamController` seam: `app.rs` gets a `trait Streamer` over `start/set_destinations/sites/state/stop`, with the real controller and a fake in tests.)

- [ ] **Step 2: Run the tests to make sure they fail.** Run `cargo test -p page2cam -p p2c-admin`.
- [ ] **Step 3: Implement** as specified. Every error text passes through `redact_all`.
- [ ] **Step 4: Run the tests to make sure they pass.** Run `cargo test -p page2cam -p p2c-admin`.
- [ ] **Step 5: Commit.** `git commit -am "engine: YouTube and Twitch stream sites (ticks, keys, start rules, per-site status, bitrate note)"`

---

### Task 5: Control page stream panel

**Files:**
- Modify: `crates/p2c-admin/ui/index.html`
- Test: `crates/p2c-admin/src/server.rs` (existing UI-serving tests: the page contains the new element ids)

**Interfaces:**
- Consumes: `Status.streams`, `Status.stream_note`, `POST /api/stream/start` (sites body), `POST /api/stream/sites`.
- Produces: the panel from the spec's sketch.
  - Element ids: `#site-youtube-on`, `#site-youtube-key`, `#site-youtube-state`, `#site-twitch-on`, `#site-twitch-key`, `#site-twitch-state`, `#stream-advanced` (server URLs), `#stream-note`.
  - A key box shows `●●●●●● (saved)` as its placeholder when `has_saved_key`.
  - Ticking while live calls `/api/stream/sites`. Errors show in the existing stream error line.

- [ ] **Step 1: Write the failing test.** Add `ui_has_stream_site_controls`: `GET /` with the token contains every id above.
- [ ] **Step 2: Run it to make sure it fails.** Run `cargo test -p p2c-admin ui_has_stream_site_controls`.
- [ ] **Step 3: Implement** the panel. No external resources, as with the existing UI rule.
- [ ] **Step 4: Run it to make sure it passes,** then check it manually in the dev build with `cargo run -p page2cam -- --standalone --test-pattern --no-open`. Open the printed URL, tick and untick, and confirm the state lines update. Use two local RTMP listeners as the servers (via Advanced).
- [ ] **Step 5: Commit.** `git commit -am "admin: YouTube and Twitch rows in the stream panel"`

---

### Task 6: Agents: stream sites over MCP and the tray

**Files:**
- Modify: `crates/p2c-mcp/src/tools.rs`, `crates/p2c-tray/src/agent_backend.rs`
- Test: the tests in both files

**Interfaces:**
- Consumes: `POST /api/stream/start` (sites body), `POST /api/stream/sites`, `Status.streams`.
- Produces:
  - **`BackendCall`:**
    - `StartStream` becomes `StartStream { sites: Option<Vec<Site>> }`;
    - `StopStream` becomes `StopStream { sites: Option<Vec<Site>> }`;
    - new `SetStreamSites { youtube: Option<bool>, twitch: Option<bool> }`.
    - Use string sites (`"youtube"|"twitch"`) in p2c-mcp to keep it free of a p2c-stream dependency.
  - **Tools:**
    - `start_stream {sites?: ["youtube","twitch"]}`;
    - `stop_stream {sites?}`;
    - `set_stream_sites {youtube?: bool, twitch?: bool}` (minProperties 1).
    - Descriptions: keys are never passed or readable; a missing key error means the user must enter it in the control page.
  - **Tray backend:**
    - `StartStream` posts `{sites: {youtube: {on}, twitch: {on}}}`. With `sites` given, the listed sites are on and the others off. Without it, the body is `{sites: {}}`, meaning the saved ticks.
    - It no longer requires `has_saved_key` itself; the engine's message comes back.
    - `StopStream{Some(..)}` posts `/api/stream/sites` with those sites off.
    - Balloon text stays as today.

- [ ] **Step 1: Write the failing tests.**
  - p2c-mcp:
    - `start_stream_takes_sites`: `{"sites":["twitch"]}` gives `StartStream{sites: Some(["twitch"])}`; `{"sites":["kick"]}` gives `isError` naming youtube and twitch.
    - `set_stream_sites_needs_one_field`.
    - The tool list snapshot includes `set_stream_sites`.
  - Tray:
    - `start_stream_posts_sites`: the log shows `/api/stream/start` with body `{"sites":{"youtube":{"on":false},"twitch":{"on":true}}}`.
    - `missing_key_message_reaches_the_agent`: the engine answers 500 `Twitch has no stream key`; the tool error text contains it.
- [ ] **Step 2: Run them to make sure they fail.** Run `cargo test -p p2c-mcp -p p2c-tray`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them to make sure they pass.** Run `cargo test -p p2c-mcp -p p2c-tray`.
- [ ] **Step 5: Commit.** `git commit -am "agents: start/stop streams per site, set_stream_sites"`

---

### Task 7: Window listing, matching and Windows Graphics Capture of a window

**Files:**
- Create: `crates/p2c-screen/src/window.rs` (platform-free types and matching), `crates/p2c-screen/src/win_window.rs` (Windows enumeration)
- Modify: `crates/p2c-screen/src/win.rs` (the session takes a `GraphicsCaptureItem` from a monitor **or** a window; cursor flag; borderless request; size-change pool recreate; status), `crates/p2c-screen/src/lib.rs`
- Test: unit tests in `window.rs`; `crates/p2c-screen/tests/window_capture.rs` (Windows, `#[ignore]`)

**Interfaces:**
- Produces:
  - `pub const WINDOW_UNSUPPORTED: &str = "window capture is Windows-only for now";`
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub struct WindowInfo { pub id: u64 /*HWND*/, pub title: String, pub exe: String /*file name, e.g. chrome.exe*/, pub app: String /*FileDescription or exe stem*/, pub pid: u32, pub width: u32, pub height: u32, pub minimized: bool }`
  - `#[derive(Clone, Debug, Default, PartialEq, Eq)] pub struct WindowMatch { pub exe: String, pub title: String }`
  - `pub fn list_windows() -> anyhow::Result<Vec<WindowInfo>>`. It lists visible top-level windows that are not tool windows and not cloaked, with a non-empty title. It excludes the own process's windows and the `Progman`/`Shell_TrayWnd` classes.
  - `pub fn find_exact<'a>(l: &'a [WindowInfo], m: &WindowMatch) -> Option<&'a WindowInfo>`: same exe (case-insensitive) and an equal title.
  - `pub fn resolve_text<'a>(l: &'a [WindowInfo], text: &str) -> Result<&'a WindowInfo, String>`. Order: an id (decimal HWND) in the list; else an exact title; else a unique case-insensitive substring. Otherwise `Err("no window matches \"<text>\" (windows: <t1>; <t2>; …)")` or `Err("\"<text>\" matches several windows: <t1>; <t2>")`.
  - `pub fn is_browser(exe: &str) -> bool` (the six exe names from Global Constraints).
  - `pub enum WindowStatus { Ok, Minimized, Closed }`
  - `Capture::start_window(id: u64, max_fps: u32, sink: Sink) -> anyhow::Result<Capture>`: cursor off, borderless requested (`GraphicsCaptureSession::IsBorderRequired(false)` after `GraphicsCaptureAccess::RequestAccessAsync(Borderless)`; on failure, keep the border and log once).
  - `Capture::window_status(&self) -> WindowStatus`, polled by the engine. Minimized comes from `IsIconic`; Closed comes from `!IsWindow` or the item's `Closed` event.
  - The frame size follows `ContentSize`: on change, `pool.Recreate` and a new staging texture.
  - Sizes below 2×2 are skipped. A minimized window produces no frames.

- [ ] **Step 1: Write the failing unit tests** (platform-free):

```rust
fn w(id: u64, exe: &str, title: &str) -> WindowInfo { /* fields, 1280x720, pid 1, not minimized */ }
#[test] fn exact_match_needs_exe_and_title() {
    let l = [w(1, "chrome.exe", "xQc - Twitch - Google Chrome"), w(2, "msedge.exe", "xQc - Twitch - Microsoft Edge")];
    let m = WindowMatch { exe: "CHROME.EXE".into(), title: "xQc - Twitch - Google Chrome".into() };
    assert_eq!(find_exact(&l, &m).unwrap().id, 1);
    let near = WindowMatch { exe: "chrome.exe".into(), title: "xQc - Twitch".into() };
    assert!(find_exact(&l, &near).is_none(), "never a near miss");
}
#[test] fn agent_text_must_pick_one() {
    let l = [w(1, "chrome.exe", "Gmail - Google Chrome"), w(2, "chrome.exe", "Twitch - Google Chrome")];
    assert_eq!(resolve_text(&l, "twitch").unwrap().id, 2);
    assert!(resolve_text(&l, "google chrome").unwrap_err().contains("matches several windows"));
    assert!(resolve_text(&l, "kick").unwrap_err().starts_with("no window matches \"kick\""));
    assert_eq!(resolve_text(&l, "1").unwrap().id, 1);
}
#[test] fn browsers_are_known() { assert!(is_browser("MSEDGE.exe") && !is_browser("game.exe")); }
#[cfg(not(windows))] #[test] fn windows_only() {
    assert_eq!(list_windows().unwrap_err().to_string(), WINDOW_UNSUPPORTED);
}
```

- [ ] **Step 2: Write the failing Windows test** (`tests/window_capture.rs`, `#[ignore]`). It creates its own window: a Win32 window class whose `WM_PAINT` fills the client area with pure green.
  - `list_windows()` contains it (by its title `p2c-window-test-<pid>`), even though it belongs to the own process. **Exception:** the own-process filter is off when env `P2C_TEST_OWN_WINDOWS=1` is set; the test sets it.
  - `start_window` delivers a frame within 3 s whose centre pixel is green (±3).
  - `SetWindowPos` to 801×457 (odd sizes) gives a frame of that client size within 3 s, with no panic (Review Focus 5).
  - `ShowWindow(SW_MINIMIZE)` gives `window_status() == Minimized` within 2 s.
  - `DestroyWindow` gives `Closed` within 2 s.
- [ ] **Step 3: Run them to make sure they fail.** Run `cargo test -p p2c-screen`, then `cargo test -p p2c-screen --test window_capture -- --ignored`.
- [ ] **Step 4: Implement.** Refactor `Session::open(id)` into `Session::open(item: GraphicsCaptureItem, cursor: bool, borderless: bool)`, with `open_monitor`/`open_window` callers. `screen.rs` users stay unchanged.
- [ ] **Step 5: Run the tests to make sure they pass,** including the existing screen tests.
- [ ] **Step 6: Commit.** `git commit -am "screen: list, match and capture application windows (cursor off, borderless where allowed, follows size)"`

---

### Task 8: Process loopback sound

**Files:**
- Create: `crates/p2c-audio/src/loopback.rs` (Windows implementation; elsewhere `start` fails with `WINDOW_UNSUPPORTED` text duplicated as a local const)
- Modify: `crates/p2c-audio/src/lib.rs`, `crates/p2c-audio/src/mixer.rs`, `crates/p2c-audio/Cargo.toml` (`windows` features: `Win32_Media_Audio`, `Win32_System_Com`, `Win32_System_Com_StructuredStorage`, `Win32_System_Variant`, `Win32_Foundation`, `Win32_System_Threading`)
- Test: `crates/p2c-audio/tests/loopback.rs` (Windows, `#[ignore]`); mixer unit test

**Interfaces:**
- Produces:
  - `pub struct ProcessLoopback`, with `pub fn start(pid: u32, ring: AudioRing) -> anyhow::Result<ProcessLoopback>`. It captures `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` of `pid` via `ActivateAudioInterfaceAsync` (`VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK`). It resamples with the existing `Resampler` to 48 kHz stereo f32 and pushes to `ring`. Dropping it stops capture.
  - `ProcessLoopback::error(&self) -> Option<String>`.
  - `Mixer::with_window(self, ring: AudioRing) -> Self`: mixed as page sound (same `page_on`/`page_gain`), like `with_overlay`.

- [ ] **Step 1: Write the failing tests**
  - Mixer unit test `window_sound_follows_page_switches`: a ring with a constant 0.5 gives 0.5 out with page on and gain 1, 0 with page off, and 1.0 with gain 2 (clamped as the existing tests do).
  - `tests/loopback.rs` `#[ignore]` `captures_only_the_target_process`:
    - spawn child A, `powershell -c "(New-Object Media.SoundPlayer '<generated 440 Hz wav>').PlaySync()"` (the test writes the wav);
    - spawn child B, the same with a 1000 Hz wav;
    - `start(A.pid)` for 2 s;
    - a Goertzel measure of the ring finds energy at 440 Hz and below 1/10 of that at 1000 Hz.
- [ ] **Step 2: Run them to make sure they fail.** Run `cargo test -p p2c-audio`, then `cargo test -p p2c-audio --test loopback -- --ignored`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them to make sure they pass.**
- [ ] **Step 5: Commit.** `git commit -am "audio: capture one process tree's sound (window background), mixed as page sound"`

---

### Task 9: Engine window background

**Files:**
- Create: `crates/page2cam/src/window.rs` (modelled on `screen.rs`)
- Modify:
  - `crates/page2cam/src/background.rs`: `FullBg::Window(WindowMatch)`, `as_str` `"window"`, `is_generated` true, `fn window(&self) -> Option<&WindowMatch>`;
  - `crates/page2cam/src/config.rs`: keys `background_window_exe`, `background_window_title`, default empty;
  - `crates/page2cam/src/app.rs`: `sync_window` beside `sync_screen`, notes, `windows()`, `BackgroundDto.full_window`;
  - `crates/page2cam/src/main.rs`: wiring.
  - `crates/p2c-admin/src/api.rs`/`server.rs`: `GET /api/windows` (`WindowDto { id, title, exe, app, width, height, minimized, browser }`, 501 `{error}` where unsupported); `BackgroundDto.full_window: Option<String>` (id or title text in, `"<title> (<exe>)"` out).
- Test: `window.rs` tests (fake backend), `app.rs` tests, `server.rs` tests.

**Interfaces:**
- Consumes: Task 7 (`WindowInfo`, `WindowMatch`, `find_exact`, `resolve_text`, `is_browser`, `Capture::start_window`, `WindowStatus`) and Task 8 (`ProcessLoopback`, `Mixer::with_window`).
- Produces:
  - `pub trait WindowBackend: Send + Sync { fn list(&self) -> anyhow::Result<Vec<WindowInfo>>; fn start(&self, w: &WindowInfo) -> anyhow::Result<Box<dyn WindowRun>>; }`
  - `pub trait WindowRun: Send { fn status(&self) -> WindowStatus; fn title(&self) -> Option<String>; fn sound_error(&self) -> Option<String>; }`
  - `pub struct WindowShare`, with:
    - `fn new(backend: Box<dyn WindowBackend>) -> Self`;
    - `fn sync(&self, want: Option<&WindowMatch>)`;
    - `fn tick(&self) -> Option<WindowMatch /*title changed: save it*/>`, called every 2 s by the engine's existing 250 ms watchdog (every 8th tick);
    - `fn note(&self) -> Option<String>`;
    - `fn list(&self)`;
    - `fn resolve(&self, text: &str) -> anyhow::Result<WindowMatch>`.
  - **Behaviour:**
    - `sync(Some(m))` starts on `find_exact`. Without an exact match there is no capture and the note is `window not found: <title> (<exe>): choose it again`; each `tick` retries `find_exact`.
    - While running, `tick` reads `status()`: `Minimized` → note; `Closed` → note `window closed`, the run is dropped, a black frame is pushed once, and matching resumes.
    - When `title()` changes, `tick` returns the new match. The app saves it with `save_values`, at most every 10 s.
    - The browser note `Sound: everything playing in <app>` shows while a browser window is captured. A sound failure note takes priority.
    - A failed start (access denied) gives the note `window capture failed: <e>` and no panic.
  - **Background interplay:** same as screen (forces `cutout-full`; Chromium paused; `NOTE_TO_PAGE` rules). `set_background` with `full: "window"` uses `full_window`: an id or text through `resolve`, else the saved match.

- [ ] **Step 1: Write the failing tests** (fake backend with scripted lists and statuses):
  - `exact_match_starts_and_near_miss_does_not`: saved `{chrome.exe, "A - Twitch"}`; the list has `{chrome.exe, "Gmail"}` → no start and the not-found note. Add `{chrome.exe, "A - Twitch"}` → the next `tick` starts it.
  - `title_change_while_running_is_followed_and_saved`: the run reports a new title → `tick` returns `Some(match with new title)`, and the capture is not restarted (start count 1).
  - `minimized_then_closed` (Review Focus 5): the notes appear in order; after Closed, a re-listed window with the **new** saved title resumes, and one with the old title doesn't.
  - `start_failure_and_sound_failure_are_notes` (Review Focus 4): start error → note `window capture failed: access denied`. Running with `sound_error` set → note `window sound unavailable: <reason>`.
  - `browser_sound_warning`: a captured `msedge.exe` gives the note `Sound: everything playing in Microsoft Edge`.
  - In `app.rs`, `window_background_pauses_the_page_and_saves`: `set_background({full:"window", full_window:"twitch"})` gives the page paused, the style `cutout-full`, and the config saved with exe and title. `background().full_window == Some("A - Twitch (chrome.exe)")`.
  - In `server.rs`, `windows_route_lists_or_says_why_not` (501 with `window capture is Windows-only for now`).
- [ ] **Step 2: Run them to make sure they fail.** Run `cargo test -p page2cam -p p2c-admin`.
- [ ] **Step 3: Implement.** The real `WindowBackend` starts `Capture::start_window` plus `ProcessLoopback::start(w.pid, window_ring)`. A loopback failure keeps the picture. The window ring is created in `main.rs` and given to the mixer with `with_window`.
- [ ] **Step 4: Run them to make sure they pass.**
- [ ] **Step 5: Commit.** `git commit -am "engine: window background (exact-match resume, follows title, minimized/closed notes, app sound as page sound)"`

---

### Task 10: Control page window choice

**Files:**
- Modify: `crates/p2c-admin/ui/index.html`
- Test: `server.rs` UI ids test

**Interfaces:**
- Consumes: `GET /api/windows`, `BackgroundDto.full_window`, `Status.background_note`.
- Produces:
  - a **Window** option in the full-frame choices;
  - `#window-pick` (a select of `title — app`), `#window-refresh`;
  - the browser warning shown under the select when the chosen entry has `browser: true`.

- [ ] **Step 1: Write the failing test.** Extend the UI ids test with `window-pick` and `window-refresh`.
- [ ] **Step 2: Run it to make sure it fails, then implement.** Check manually in the dev build: pick a Notepad window, then an Edge window (the warning shows).
- [ ] **Step 3: Run it to make sure it passes, then commit.** `git commit -am "admin: Window background choice"`

---

### Task 11: Agents: windows over MCP and the tray

**Files:**
- Modify: `crates/p2c-mcp/src/tools.rs`, `crates/p2c-tray/src/agent_backend.rs`
- Test: the tests in both files

**Interfaces:**
- Consumes: `GET /api/windows`, `BackgroundDto.full_window`.
- Produces:
  - **Tools:**
    - `list_windows`, described as: "The open application windows the window background can show (title, app, exe, size, minimized, browser). Titles can be private: list them only when the user asked for a window background." It maps to `BackendCall::ListWindows`.
    - `set_background` gains `full: "window"` and `window` (string ≤ 200: an id from `list_windows` or title text).
  - **Description additions:** a browser window's sound is everything playing in that browser; suggest a separate browser for the content; games need borderless or windowed mode.
  - **Tray:**
    - The balloon `Agent “<name>” is sharing a window: <title>` always shows on `set_background` with `full: "window"`, and on a scene load whose `background_full == "window"`.
    - The agent answer includes the engine's note (so the browser warning reaches the agent).

- [ ] **Step 1: Write the failing tests.**
  - `set_background_window_args`: `{"full":"window","window":"Twitch"}` is passed through as `full_window`; `window` over 200 characters gives `isError`.
  - `list_windows_tool_exists`.
  - Tray `window_sharing_is_announced`: the balloon text, mirroring `screens_are_listed_and_screen_sharing_is_announced`.
- [ ] **Step 2: Run them to make sure they fail.** Run `cargo test -p p2c-mcp -p p2c-tray`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them to make sure they pass.**
- [ ] **Step 5: Commit.** `git commit -am "agents: list_windows, window background, 'sharing a window' balloon"`

---

### Task 12: Guide, docs and project notes

**Files:**
- Create: `crates/p2c-guide/content/playbooks/reaction-streams.md` (group `creators`; aliases `reaction, watch along, watch party, twitch, react, gaming stream`)
- Modify:
  - `crates/p2c-guide/content/instructions.md` (one line: reactions or games → guide `reaction-streams`; must stay ≤ 60 lines);
  - `crates/p2c-guide/content/howto/layout.md` or `privacy.md` (window background and browser sound);
  - `docs/agent-api.md`;
  - `README.md` (feature bullets: YouTube + Twitch at once; window background);
  - `CLAUDE.md` (architecture: fan-out and senders, sites, `FullBg::Window`, `window.rs`, `ProcessLoopback`, new settings keys, `/api/windows`, `/api/stream/sites`);
  - `docs/agent-guide/` (regenerated).
- Test: `cargo test -p p2c-guide` (each playbook's `add_layer` lines are checked against a real `LayerSet`; docs sync).

**Interfaces:**
- Consumes: the tool names from Tasks 6 and 11.
- Produces: playbook sections Background / Webcam / Graphics / Steps / Rules. The steps:
  1. `list_windows`;
  2. `set_background {full: window, window: …}`;
  3. cut-out full;
  4. `set_stream_sites`;
  5. `start_stream`.

  Rules:
  - the user listens in their own browser;
  - use a second browser for the content, and never have a call in it;
  - only rebroadcast content they have the right to use;
  - games in borderless or windowed mode.

- [ ] **Step 1: Write the playbook and docs.**
- [ ] **Step 2: Regenerate and run.** `P2C_WRITE_DOCS=1 cargo test -p p2c-guide --test docs_sync`, then `cargo test -p p2c-guide`. Expected: pass.
- [ ] **Step 3: Commit.** `git commit -am "guide: reaction and watch-along streams playbook; docs for Twitch and window background"`

---

### Task 13: Full verification on the laptop

**Files:** none (verification only; fix in the owning task's files if anything fails)

- [ ] **Step 1: Run the whole suite.** `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`. Expected: clean.
- [ ] **Step 2: Run the ignored tests.**
  - `cargo test -p p2c-stream --test rtmp_local -- --ignored`
  - `cargo test -p p2c-screen --test window_capture -- --ignored`
  - `cargo test -p p2c-audio --test loopback -- --ignored`
  - Expected: all pass.
- [ ] **Step 3: Build the package.** `./scripts/package-windows.sh build`. Expected: "Assembled …".
- [ ] **Step 4: Hand over to the user** for the real check (they run the installed or dev build themselves):
  - YouTube and Twitch at once with real keys, and one site dropping;
  - Twitch in Edge as the window background with its sound on the stream;
  - a game window in borderless mode.

  Report the results faithfully.
