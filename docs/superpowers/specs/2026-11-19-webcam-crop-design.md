# Webcam crop — design (short)

Date: 2026-11-19. Status: awaiting user approval.

## Why
When placing the webcam overlay the operator can move and resize it but always gets the whole camera picture. They want to cut away parts of it — the room at the sides (Box), the desk at the bottom (full-height cut-out), or zoom in on their face — in every overlay style.

## Goals
- Trim any edge (left/top/right/bottom) and zoom in (with pan), in Box, Cut-out small and Cut-out full.
- Edit it directly on the admin preview, plus a zoom slider and "Reset crop".
- Saved in the settings file; survives restarts.
- The cut-out model sees the cropped picture (closer person → sharper outline).

## Non-goals / anti-goals
- No rotation, no non-rectangular crop, no per-style crop presets.
- Must not cost noticeable CPU/GPU (crop is a cheap row copy) or add latency.
- A crop must never make the overlay disappear: minimum crop size 10 % of the camera width/height.

## Approaches (user picked A)
- **A (chosen): one crop rectangle applied right after capture.** The capture thread (and `--fake-webcam`) cuts each decoded frame to the rectangle before putting it in the webcam slot, so the segmenter, both compositors and the admin geometry (which already follow the webcam aspect) work unchanged on a smaller frame.
- B: crop inside the CPU and GPU compositors — rejected: more code in both paths and shaders, and the model still sees the whole room.
- C: like A with sliders only — rejected: clumsy while presenting.

## Design
- `p2c_core::Crop { x, y, w, h }` in 0..1 of the camera frame (default `{0,0,1,1}`), with `Crop::clamp()` (min 0.10, inside the frame) and `Crop::pixels(cam_w, cam_h) -> (x, y, w, h)` with even sizes (NV12/MJPEG friendliness, `camera_size` rules).
- Shared `Arc<RwLock<Crop>>`; the capture thread reads it per frame; a non-full crop copies the rows of the cropped region into a new `Frame` (≈ few hundred µs at 720p). Full crop → no copy.
- Changing the crop changes the webcam frame size → the segmenter already resets the Quality model on size change (edge settles in ~0.5 s; acceptable while adjusting).
- Config: `webcam_crop = [x, y, w, h]` (default full), saved with `toml_edit` like `overlay_style`.
- Admin API: `GET`/`POST /api/crop` `{x, y, w, h}` (token + Host guard like every route); `Status.webcam_aspect` reports the **cropped** aspect so the UI geometry follows.
- Admin UI (`ui/index.html`): a "Crop" toggle on the overlay panel. In crop mode the overlay box shows edge handles: dragging an edge inward/outward trims/untrims that side (the overlay shrinks/grows accordingly); a zoom slider (1×–4×) shrinks the crop around its centre keeping the overlay's aspect; dragging inside the box pans; "Reset crop" returns to full. Outside crop mode drag/resize behave as today. Changes are sent live (throttled ~10/s) and saved when the drag ends.

## Left to the builder
- Exact handle hit areas/visuals (theme colours from CLAUDE.md), throttling details, where `Crop` lives if p2c-core is not the natural home.
