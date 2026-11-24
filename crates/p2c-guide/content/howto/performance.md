---
title: Performance: fit Page2Cam to this PC
aliases: slow, choppy, laggy, stutter, fps, frame rate, resolution, cpu, gpu, laptop, battery, optimise, optimize, tune, settings, power
---
# Performance: fit Page2Cam to this PC

Do this once after installing, and again when the user says the picture is choppy, late or the fan is loud. Never tune during a live call or stream without asking: some steps restart Page2Cam.

## 1. Measure

`start` if stopped, show what the user normally shows, wait 30 s, then `status`. Read:

- `status.health.output_skipped_pct`: output frames missed. **≤ 2 good**, 2–10 strained, > 10 choppy.
- `status.health.webcam_fps`: **≥ 15 good**, < 12 looks jerky (often low light or a USB hub, not the PC).
- `status.cutout`: e.g. "Quality · GPU · 24 masks/s"; `status.health.cutout_auto_fallback` true = Quality was too slow and switched to Fast by itself.
- `status.video`: "GPU: <name>" is good; "CPU (fallback: …)" means the GPU path failed.
- `status.health.video_fallback`, `status.gpu_override`: true = something fell back to the CPU; a `force_restart` retries the GPU.
- `status.width`, `status.height`, `status.fps`: the output size and rate.

Page2Cam already helps itself: when output skips, it lowers the page's frame rate and then its render size (logged "page quality changed"), and the cut-out falls back from Quality to Fast. Tune when those are not enough, or to stop them happening.

## 2. Change one thing at a time, cheapest first

Check `status` again 30 s after each change. Stop as soon as `output_skipped_pct` ≤ 2.

1. **Heavy page**: a WebGL/3D/video-heavy website costs the most. Try `show_page` with a still design, an image or colour background (`set_background`), or ask the user for a lighter page.
2. **Refined edges off**: `set_background {"refined_edges": false}`. Costly on onboard (Intel/AMD integrated) graphics.
3. **Blurred room off** if not needed: the blur is recomputed for every webcam frame.
4. **Cut-out Fast**: `set_cutout_quality {"quality": "fast"}`. Quality needs a real GPU (about 13 ms a frame on an NVIDIA laptop GPU, 60 ms or more on onboard graphics).
5. **Box instead of a cut-out**: `set_overlay {"style": "box"}` needs no cut-out model at all (unless the box background is blur/image/colour).
6. **The right GPU** (laptops with two GPUs): `list_gpus`, then `set_gpu {"choice": "<exact name of the discrete GPU>"}` (restarts Page2Cam). `auto` usually picks it; `cpu` only when the GPU keeps failing.
7. **Smaller or slower output** (settings file, restart needed, see below): `width = 1280`, `height = 720`, then `fps = 24` or `20`. Call apps send 720p anyway on most calls; streams look fine at 720p.

## 3. The settings file (only for size, frame rate, stream bitrate)

Output size, frame rate and stream bitrate have no tool; they live in the settings file:

- installed copy: `%APPDATA%\page2cam\config\config.toml`
- portable zip: `page2cam.toml` next to `page2cam.exe` if it exists, else the same file as above.

Rules: `stop` first (Page2Cam rewrites this file while running), edit only these keys, keep every other line, then `start`. Unknown keys or bad values stop Page2Cam from starting (the tray turns red; the log says which key).

| Key | Default | Allowed | Lighter |
|---|---|---|---|
| `width`, `height` | 1920, 1080 | even, 320–3840 × 240–2160 | 1280, 720 |
| `fps` | 30 | 1–60 | 24 or 20 (15 for slides only) |
| `video_kbps` | 6000 | stream bitrate | 3500 at 720p, or on a slow upload |
| `cutout_engine` | "quality" | "quality", "fast" | "fast" |
| `refined_edges` | false | true, false | false |
| `gpu` | "auto" | "auto", "cpu", exact name from `list_gpus` | |

## 4. The PC itself

- Laptops: plugged in, Windows power mode **Best performance** (Settings, System, Power). On battery or "Best power efficiency", Windows caps the GPU and Page2Cam may halve its speed.
- Close other heavy apps (games, renders, many browser tabs with video). Teams/Zoom themselves use a lot: their own background effects should be **off** (Page2Cam already does the background).
- Webcam: good light raises its frame rate more than any setting.

## 5. Tell the user

Say what you measured (skipped %, webcam fps, cut-out rate, GPU), what you changed and why, and what it costs them (e.g. "720p instead of 1080p", "Fast cut-out: slightly rougher hair edges"). If they want something back, change it back.
