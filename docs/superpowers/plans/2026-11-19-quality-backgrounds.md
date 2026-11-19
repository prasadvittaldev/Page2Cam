# Cut-out Quality and Backgrounds Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** User-selectable refined cut-out edges, full-frame backgrounds (Page / Image / Colour / YouTube / Blurred room), in-box backgrounds (Room / Blur / Image / Colour), and an automatic CPU saving from NV12 capture.

**Architecture:** Full-frame Image/Colour/YouTube are generated HTML pages served by the admin server at an unguessable per-launch path and shown in Chromium like any page. Blurred room, in-box backgrounds and refined edges (guided upsampling of the low-res mask, guided by the webcam frame) are drawn by the compositors — CPU (`p2c-compositor`) and D3D11 (`p2c-d3d`) — from a shared `Backdrop` setting updated live like `PipLayout`. Capture prefers NV12 and also publishes it to the segmenter.

**Tech Stack:** Rust, D3D11 HLSL (runtime-compiled as today), axum, toml_edit, image crate.

**Spec:** `docs/superpowers/specs/2026-11-19-quality-backgrounds-design.md` (approved 2026-11-19; its "Decisions settled after the builder check" override the sections below them).

## Global Constraints

- Settings keys (settings file, written with `toml_edit`, comments kept): `background = "page"|"image"|"colour"|"youtube"|"blurred-room"` (default page), `background_image`, `background_colour = "#rrggbb"`, `background_youtube`, `box_background = "room"|"blur"|"image"|"colour"` (default room), `box_background_image`, `box_background_colour`, `refined_edges = false`, `last_page_url`.
- Refined edges off = today's output exactly (EdgeRange LUT path unchanged). On = guided upsampling of the model-size `Mask` using the webcam frame as guide, EdgeRange LUT skipped. Budget ≤ 2 ms GPU per frame, ≤ 6 ms CPU per new mask (CPU path at the PIP drawn size, ≤ 960 px wide).
- No full-resolution mask crosses threads; the segmenter keeps publishing the model-size `Mask` to `MaskSlot`.
- Blur: recomputed only when a new webcam frame arrives, at reduced resolution, cached like the PIP layer; on both CPU and GPU paths.
- Fallbacks: in-box Blur without a fresh mask → whole box blurred; in-box Image/Colour without a fresh mask → raw room as Box + note; Blurred room without a fresh mask → whole frame blurred + note. Never the sharp room under a blur setting.
- Interplay: Box chosen while full-frame ≠ page → full-frame becomes page (note). Full-frame ≠ page chosen while Box → style becomes cutout-full, saved.
- Background pages: `http://127.0.0.1:<admin port>/bg/<per-launch random 32-hex id>/current.html`, no token, Host check kept, no secrets in the page; Chromium switch `--autoplay-policy=no-user-gesture-required`.
- YouTube: official embed `https://www.youtube-nocookie.com/embed/<id>?autoplay=1&loop=1&playlist=<id>&controls=0&modestbranding=1&playsinline=1`; ids are 11 chars `[A-Za-z0-9_-]`, parsed from watch, youtu.be, shorts, embed, live links (timestamps/playlist params ignored); anything else rejected with a message.
- Images: upload ≤ 20 MB, PNG/JPEG/WebP only (sniffed, not by extension), re-encoded to PNG/JPEG into `<data dir>/backgrounds/{full,box}.<ext>`; one image per row, replaced on upload.
- The admin UI stays free of external resources (YouTube thumbnail fetched by the engine, served from `/api/background/thumb`, token-guarded).
- 30 fps must hold on the user's laptop with Quality + refined edges + Blur.
- Windows-only code `cfg(windows)`; workspace builds on Linux and in `cargo xwin`; the CPU path covers every mode. Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Don't run app instances while the user's Page2Cam runs.

## Review Focus

1. **Webcam frame size changes or crop changes while Blur/refined is on** (camera reconnect, crop drag) — no stale cached blur at the wrong size, no panic. Test in Task 2 (`blur_cache_rebuilds_on_size_change`) and Task 4 (GPU equivalent).
2. **Mask and webcam frame from different moments / sizes** (mask is model-size, webcam 1280×720, crop applied) — guided filter scales correctly, never reads out of bounds. Test in Task 2 (`guided_handles_any_mask_and_guide_sizes`).
3. **Bad YouTube links and pages that won't play** (private video, playlist-only link, plain text) — clear message, page unchanged. Test in Task 5 (`youtube_id_parsing_table`).
4. **Huge or fake image upload** (25 MB, a .png that is really HTML, 10000×10000) — rejected or downscaled, never written as-is. Test in Task 5 (`image_upload_limits`).
5. **Switching quickly between backgrounds and Page** — the user's page URL is restored, never overwritten by a background URL. Test in Task 5 (`last_page_url_never_becomes_a_background_url`).

---

### Task 1: NV12 capture for the cut-out

**Files:** `crates/p2c-webcam/src/{format.rs,capture.rs}`, `crates/p2c-core/src/nv12.rs` (slot), `crates/p2c-segment/src/{segmenter.rs,tensor.rs}`, `crates/page2cam/src/main.rs` (wiring)

**Interfaces:**
- Produces: `choose_format` prefers NV12 over MJPEG at the same size and fps (other ranking unchanged); `p2c_core::Nv12Slot` (latest-only, `seq`, same API shape as `FrameSlot`); capture puts the cropped NV12 frame into it when the camera delivers NV12 (BGRA path unchanged); segmenter builds the model input from NV12 when the slot's `seq` matches the BGRA frame it is processing, else from BGRA.
- [ ] **Step 1: Failing tests:** `choose_format_prefers_nv12_at_same_mode` (the user's mode list from CLAUDE.md known risks / logs: NV12 and MJPEG 1280×720@30 → NV12), `choose_format_keeps_mjpeg_when_it_is_faster` (MJPEG 30 vs NV12 15 → MJPEG), `nv12_crop_matches_bgra_crop`, `nv12_model_input_matches_bgra_path` (±2 levels per channel on a synthetic frame).
- [ ] **Step 2–4:** run → fail; implement; `cargo test --workspace`, clippy, fmt; log segmenter preprocessing ms in the existing 30 s stats line.
- [ ] **Step 5: Commit** `perf(webcam): prefer NV12 capture and feed it to the cut-out`.

### Task 2: CPU compositor — guided filter and blur

**Files:** Create `crates/p2c-compositor/src/{guided.rs,blur.rs}`; modify `compose.rs`, `lib.rs`

**Interfaces:**
- Produces: `fn guided_upsample(mask: &Mask, guide: &Frame, out_w: u32, out_h: u32, radius: u32, eps: f32) -> Vec<u8>` (alpha 0..255 at out size; guide = webcam BGRA, luma used); `struct BlurCache` (`fn get(&mut self, cam: &Arc<Frame>, w: u32, h: u32) -> &[u8]`, rebuilds on frame seq/size change; separable Gaussian at ≤ 1/4 resolution, radius ≈ 2 % of width, upscaled bilinear).
- [ ] **Step 1: Failing tests:** `guided_keeps_hard_edge_sharp` (step edge in guide + blurry mask → 10–90 % transition ≤ 3 px), `guided_flat_stays_flat`, `guided_handles_any_mask_and_guide_sizes` (256×144 mask, 1280×720 / 1×1 / odd-size guides; no panic, output len = w×h), `guided_speed_1280x720` (ignored; prints ms), `blur_is_smooth_and_same_size`, `blur_cache_rebuilds_on_size_change`, `blur_cache_reuses_same_frame`.
- [ ] **Step 2–4:** run → fail; implement; tests, clippy (incl. Linux target), fmt.
- [ ] **Step 5: Commit** `feat(compositor): guided-filter edges and blur on the CPU path`.

### Task 3: CPU compositor — backdrops

**Files:** `crates/p2c-compositor/src/{compose.rs,pip.rs,lib.rs,ticker.rs}`

**Interfaces:**
- Consumes: Task 2.
- Produces: `enum FullBackdrop { Page, BlurredRoom }`; `enum BoxBackdrop { Room, Blur, Image(Arc<Frame>), Colour([u8; 3]) }`; `struct Backdrop { full: FullBackdrop, in_box: BoxBackdrop, refined_edges: bool }` (Default = Page, Room, false); shared as `Arc<RwLock<Backdrop>>` next to the layout and read each tick; `Compositor::compose` takes `&Backdrop`; status note via a new `ComposeNote` (`None | CutoutNotReady`) the ticker exposes for `Status.background_note`.
- Behaviour: Box + Room = today; Box + Blur/Image/Colour = background in the PIP rect then the person by matte (fallbacks per Global Constraints); cut-out styles ignore `in_box`; BlurredRoom = blurred webcam cover-scaled over the whole frame instead of the page, then the PIP/cut-out as usual (no fresh mask → whole frame blurred); refined_edges switches the matte from LUT to `guided_upsample`.
- [ ] **Step 1: Failing tests:** table test `backdrop_table` (each in_box × {mask fresh, stale} on Box; BlurredRoom × {fresh, stale}; pixel probes inside/outside the person), `refined_off_is_bit_identical_to_today` (existing compose test images), `cutout_styles_ignore_box_backdrop`, `blurred_room_never_shows_sharp_room`.
- [ ] **Step 2–4:** run → fail; implement; tests, clippy, fmt.
- [ ] **Step 5: Commit** `feat(compositor): full-frame and in-box backdrops`.

### Task 4: D3D11 compositor — the same on the GPU

**Files:** `crates/p2c-d3d/src/{compose.rs,shaders.rs}`, `crates/p2c-d3d/src/hlsl/{compose.hlsl,blur.hlsl,guided.hlsl}`

**Interfaces:**
- Consumes: Task 3 `Backdrop`, `ComposeNote`; existing `GpuProducer`, PIP layer cache, readback.
- Produces: GPU equivalents: guided upsampling pass (mask texture + webcam texture → alpha), blur pass (down-sample, separable Gaussian, cached per webcam frame), in-box and BlurredRoom drawing; image backdrops uploaded once per change; same notes as the CPU path.
- [ ] **Step 1: Failing tests (Windows, real adapter; skip with a log line when no adapter):** `gpu_backdrop_matches_cpu` (each Task 3 table case, mean abs diff ≤ 3 levels, max ≤ 24), `gpu_refined_matches_cpu`, `gpu_blur_cache_rebuilds_on_size_change`, `gpu_refined_and_blur_within_budget` (ignored; prints ms per frame at 1080p output).
- [ ] **Step 2–4:** run → fail; implement; tests, clippy (Windows + `cargo xwin` note), fmt.
- [ ] **Step 5: Commit** `feat(d3d): backdrops and refined edges on the GPU`.

### Task 5: Background pages, settings and admin API

**Files:** Create `crates/page2cam/src/background.rs`; modify `crates/page2cam/src/{config.rs,app.rs,main.rs}`, `crates/p2c-admin/src/{api.rs,server.rs}`, `crates/p2c-browser/src/lib.rs` (autoplay switch)

**Interfaces:**
- Consumes: Task 3 `Backdrop`.
- Produces: `fn youtube_id(input: &str) -> Result<String, String>`; `fn background_html(kind: &FullBg, image_url: Option<&str>) -> String` (Colour: solid; Image: `object-fit: cover` of `/bg/<id>/image`; YouTube: the embed per Global Constraints; all HTML-escaped, no other external resources); admin routes `GET /bg/<id>/current.html`, `GET /bg/<id>/image` (unguarded except Host + id), `GET/POST /api/background` (`{ full, full_colour?, full_youtube?, in_box, box_colour?, refined_edges }`), `POST /api/background/image?row=full|box` (body = image bytes), `GET /api/background/thumb` (cached YouTube thumbnail fetched by the engine); `Status` gains `background`, `box_background`, `refined_edges`, `background_note`; `Controls` gains `background() -> BackgroundDto`, `set_background(BackgroundDto) -> anyhow::Result<()>`, `set_background_image(row, bytes) -> anyhow::Result<()>`; app applies interplay rules, remembers `last_page_url` (only http/https/file user URLs, never `/bg/` URLs), navigates Chromium, updates `Backdrop`, saves settings.
- [ ] **Step 1: Failing tests:** `youtube_id_parsing_table` (watch?v=, youtu.be/, shorts/, embed/, live/, with `&t=30s`/`&list=`; rejects `https://youtube.com/playlist?list=…`, `hello`, other hosts), `background_html_escapes_and_has_no_other_external_urls`, `image_upload_limits` (25 MB → 413, HTML bytes named .png → 415, 10000×10000 → downscaled ≤ 3840 wide), `last_page_url_never_becomes_a_background_url`, `box_switch_resets_full_background` and `full_background_in_box_switches_to_cutout_full_and_saves`, `bg_route_needs_id_and_host`, `settings_roundtrip_keeps_comments`.
- [ ] **Step 2–4:** run → fail; implement; tests, clippy, fmt.
- [ ] **Step 5: Commit** `feat: background pages, settings and admin API`.

### Task 6: Admin page Background section, docs

**Files:** `crates/p2c-admin/ui/index.html`, `CLAUDE.md`, `packaging/windows/README.txt`

- [ ] "Background" section: *Full frame* row (Page, Image, Colour, YouTube, Blurred room) and *In box* row (Room, Blur, Image, Colour) with previews (image thumbnail, colour swatch, YouTube thumbnail via `/api/background/thumb`), image picker (uploads), colour picker, YouTube link box with the parse error shown inline, `background_note` shown; Cut-out section gains "Refined edges" switch with the measured cost from the stats. Test: admin `ui_has_background_section` (served HTML contains the ids). CLAUDE.md: fix the outdated "mask in the alpha byte" text (model-size `Mask` in `MaskSlot`, upscaled by the compositors), document Backdrop, background pages, NV12 slot, settings keys. Commit `feat(admin): Background section; docs`.

### Task 7: Acceptance (with the user)

- [ ] Ask the controller before running anything (user's Page2Cam must be closed). On the laptop: each full-frame and in-box option; YouTube with sound on a test stream (not heard in Teams); refined edges on/off side by side in the Camera app; CPU % and segmenter ms before/after NV12 (stats lines); 30 fps held (skipped ≤ 2 %) with Quality + refined + Blur. Record numbers in the ledger.
