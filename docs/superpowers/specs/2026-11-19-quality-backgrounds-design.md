# Cut-out quality, background modes and CPU savings — design

Date: 2026-11-19. Status: awaiting user approval (builder check done).

## Why
The user found the cut-out edges rough and wants more control over what is behind them: today the web page is behind a cut-out person, and the raw room is inside the webcam box. They asked for three things, **all as options the user chooses**: refined edges, background modes (including a full-screen YouTube video from a link), and automatic CPU savings. This round comes before agent control so the MCP tools can cover these features (agent-control plan resumes at its Task 2 afterwards).

## Goals
- **Refined edges**: an on/off option that sharpens the cut-out mask around hair and shoulders with a guided filter (edge-aware upsampling of the model's mask using the webcam image as guide).
- **Full-frame background** (what fills the frame behind the person): **Page** (today), **Image**, **Colour**, **YouTube** (user pastes a link; full screen, looping, official embedded player), **Blurred room** (the webcam's own room, blurred, full screen).
- **In-box background** (behind the person inside the webcam box): **Room** (today), **Blur**, **Image**, **Colour**.
- **Automatic CPU savings** (no switch): the cut-out takes the webcam's native NV12 when the camera delivers it (no BGRA round trip), and on the GPU path the model-input resize/convert happens on the GPU.
- Admin page "Background" section with two rows (Full frame / In box) and small previews; settings persist in the settings file (comments kept, as `overlay_style` today).

## Non-goals
- Downloading YouTube videos or extracting streams (only the official embed player — ToS).
- New segmentation models (RVM and MediaPipe stay; the guided filter improves their output).
- Video files as backgrounds (Image only for now; YouTube covers moving backgrounds).
- Linux GPU path (Linux keeps the CPU compositor; all modes must work there via the CPU path).

## Anti-goals
- A background option that silently shows the real room (e.g. cut-out falls behind or fails) without the user knowing — fall back visibly (admin status line) and, for in-box Blur, prefer blurred room over sharp room.
- Background choices making the camera stutter: the 30 fps output must hold on the user's laptop with Quality + refined edges + Blur on.
- YouTube sound reaching call apps (page audio goes to the stream only, as today).
- Losing the user's page: switching back to Page returns to the URL they had.

## Approaches (user picked A)
- **A (chosen): full-frame Image/Colour/YouTube as built-in pages; Blurred room, in-box effects and refined edges in the compositor/segmenter.** Page2Cam generates small local pages and shows them in its Chromium like any page; YouTube needs Chromium anyway, so all full-frame options share one path (and agents can later show their own backgrounds the same way). Cost: switching back to Page reloads the user's page (logins/scroll may need redoing).
- B: everything as compositor layers (page keeps running underneath). Instant switching, but about twice the drawing code (GPU + CPU) and YouTube still replaces the page.

## Design

### Decisions settled after the builder check (designer's calls — they override the sections below where they differ)
1. **Refined edges lives where the mask is upscaled.** The segmenter keeps publishing the low-res `Mask` (`MaskSlot`, model size). On the Windows GPU path the D3D compositor does the guided upsampling in a shader, using the webcam texture it already has as the guide; on the CPU path (Linux, GPU fallback) the CPU compositor does it at the PIP's drawn size (≤ 960 px wide, then bilinear). When refined edges is on, the `EdgeRange` tightening LUT is skipped (the guided filter replaces it); off = today's behaviour exactly. No full-resolution mask crosses threads. Budget: ≤ 2 ms GPU per frame, ≤ 6 ms CPU per new mask. (CLAUDE.md's "mask in the alpha byte" text is outdated; the plan fixes it.)
2. **NV12 saving = prefer NV12 capture.** `choose_format` prefers NV12 over MJPEG at the same size and fps (saves the MJPEG decode, the bigger cost). Capture keeps publishing BGRA for the overlay and additionally puts the cropped NV12 into a new latest-only NV12 slot; the segmenter scales from it when present (BGRA otherwise).
3. **GPU preprocessing for the model is dropped from this round.** The segmenter runs on its own thread with DirectML on another adapter than the compositor, and the readback is two frames old; the saving is doubtful and would add lag to the mask. Revisit only if measurements after item 2 still show the segmenter's preprocessing as a real CPU cost.
4. **Background pages are served over http, not file://.** The admin server serves the generated background at `http://127.0.0.1:<port>/bg/<per-launch random id>/current.html` (no token — the unguessable path plus the existing Host check guard it; the page contains no secrets) so the YouTube embed gets an http origin and referrer. Chromium gets `--autoplay-policy=no-user-gesture-required` for all pages: page sound only goes to the stream, never to speakers or calls, and autoplay is what a broadcast tool wants.
5. **Blur exists on both paths.** D3D shaders and CPU code; the blurred image is recomputed only when a new webcam frame arrives (cached like the PIP layer), at reduced resolution.
6. **Blurred room without a fresh mask** shows the whole frame blurred (person blurred too) plus a status note — never the sharp room.
7. **Style/background interplay.** Picking Box while a full-frame background other than Page is active switches the full-frame background back to Page (status note). Choosing a full-frame background while in Box switches the style to `cutout-full`, and that change **is saved** (it follows from the user's choice; only the automatic engine fallback stays unsaved).
8. **Small ones.** `last_page_url` is saved in the settings file (survives restarts). An uploaded image replaces the previous one (one image per row: full-frame and in-box). Every setting here gets an agent tool when the agent-control plan resumes.

### Full-frame background
- Setting `background = "page" | "image" | "colour" | "youtube" | "blurred-room"` plus `background_image` (path), `background_colour` (`#rrggbb`), `background_youtube` (URL or video id).
- **Image / Colour / YouTube**: Page2Cam writes a generated HTML file into its data dir (`backgrounds/current.html`) and navigates Chromium to it (`file://`). Image: the picked image copied into `backgrounds/` and shown `object-fit: cover`. Colour: solid body. YouTube: the official `youtube-nocookie.com/embed/<id>?autoplay=1&mute=0&loop=1&playlist=<id>&controls=0&modestbranding=1&playsinline=1` iframe at 100 % viewport (video id parsed from watch/shorts/youtu.be/embed links; anything else rejected with a message). If autoplay with sound is blocked, Chromium's autoplay policy is relaxed for our own `file://` origin via the CEF command-line switch `--autoplay-policy=no-user-gesture-required`.
- **Page**: the last user URL is remembered (`last_page_url`) when switching away and restored when switching back. The admin page's URL box keeps showing the user's page URL while a background is active, labelled "(paused — background showing)".
- **Blurred room**: the compositor fills the frame with the webcam frame scaled to cover and blurred (separable Gaussian, radius ~2 % of width, at reduced resolution then upscaled), instead of the page. Chromium is left on the last page but its frames are not drawn.
- Any full-frame option other than Page requires a cut-out style; choosing one while the style is Box switches the style to `cutout-full` and says so in the status line (the person over a full-screen background is the point).

### In-box background
- Setting `box_background = "room" | "blur" | "image" | "colour"` plus `box_background_image`, `box_background_colour`. Applies to the Box style only (in cut-out styles there is no box).
- Compositor: inside the PIP rect, draw the chosen background, then the person on top using the matte (as the cut-out styles blend today). Blur as above, computed on the webcam frame.
- Needs the matte: any value other than Room turns segmentation on. Without a fresh matte: Blur → whole box blurred (person too) rather than sharp room; Image/Colour → raw room as Box (as cut-outs fall back today); status line says "cut-out not ready".

### Refined edges
- Setting `refined_edges = true | false` (default false until measured on the user's laptop; then the default may flip).
- In `p2c-segment` post-processing: fast guided filter (He et al.) upsampling the model mask (RVM 640×360 / Fast 256×144) to the webcam resolution, guided by the webcam luma; radius 8 px at the guide resolution, eps 1e-4 (tunable constants); runs on the segmenter thread (CPU, box filters via integral images, subsampled by 2–4). The result goes into the matte alpha exactly as today.
- Budget: ≤ 6 ms per mask at 1280×720 on the user's laptop; if slower, the status line shows the cost and the user can switch it off.

### Automatic CPU savings
- **NV12 to the cut-out**: when the webcam delivers NV12 (the user's camera does), the segmenter scales directly from NV12 (Y + UV) to the model input, skipping the NV12→BGRA conversion for the model path. The overlay still needs BGRA for composition (unchanged).
- **GPU preprocessing** (Windows D3D11 path only): the webcam frame is uploaded once; a compute/pixel shader produces the model-input tensor (resize + normalise) and the guided-filter guide image; the result is read back with the existing readback ring (N−2). CPU path unchanged on Linux and GPU fallback.
- Both are internal; measured before/after (segmenter CPU ms per frame, process CPU %) and logged in the 30 s stats line.

### Admin page
- "Background" section: *Full frame* row (Page, Image, Colour, YouTube, Blurred room) and *In box* row (Room, Blur, Image, Colour) as buttons with small previews (thumbnail of the image, swatch of the colour, YouTube thumbnail from `img.youtube.com/vi/<id>/mqdefault.jpg` — the admin page must stay free of external resources, so the thumbnail is fetched by the engine and served from `/api/background/thumb`), an image picker (upload via the admin API, max 20 MB, PNG/JPEG/WebP), a colour picker, a YouTube link box.
- Cut-out section gains the "Refined edges" switch with its measured cost ("+4 ms").
- New admin routes: `GET/POST /api/background`, `POST /api/background/image` (upload), `GET /api/background/thumb`; `Status` gains `background`, `box_background`, `refined_edges`, `background_note` (fallback text).

## Testing
- Unit: YouTube id parsing (watch, youtu.be, shorts, embed, with timestamps/playlists; rejects others); generated HTML (escaping, no external resources beyond the YouTube embed); settings round trip with comments kept; guided filter on synthetic images (a hard edge stays sharp, flat areas stay flat, output range 0..255, speed test on 1280×720 ignored-by-default); NV12 direct scaling equals the BGRA path within ±2 levels; compositor in-box Blur/Image/Colour with and without a matte (CPU path); GPU shaders compared to the CPU path within tolerance (Windows ignored test).
- Admin: new routes guarded; upload size/type limits.
- Acceptance with the user: each full-frame and in-box option on the laptop, YouTube with sound on a test stream (not in Teams), refined edges on/off side by side, CPU % before/after the savings, 30 fps held.

## Left to the builder
- Exact blur kernel and resolution, guided-filter constants within the budget, shader structure, thumbnail sizes, admin layout details, whether NV12 direct scaling lives in `p2c-segment` or `p2c-core`.
